#!/usr/bin/env python3
"""
Debug tool for opencode integration research.

Spawns opencode in different modes, captures and analyzes output,
translates NDJSON events into codex-ctl log format.

Usage:
    # JSON mode (structured output)
    python3 examples/opencode_debug.py json "create hello.py" --dir /tmp/test

    # Text mode (default run format, via PTY)
    python3 examples/opencode_debug.py text "create hello.py" --dir /tmp/test

    # ACP mode (Agent Client Protocol, JSON-RPC over stdio)
    python3 examples/opencode_debug.py acp "create hello.py" --dir /tmp/test

    # Compare all modes side by side
    python3 examples/opencode_debug.py compare "create hello.py" --dir /tmp/test
"""

import argparse
import json
import os
import pty
import re
import select
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


# -- codex-ctl log message types (matching src/log/mod.rs) --

class MsgType(Enum):
    AGENT_OUTPUT = "agent_output"
    BLOCK = "block"
    STATUS = "status"
    STATE_CHANGE = "state_change"
    USER_INPUT = "user_input"


@dataclass
class LogMessage:
    seq: int
    msg_type: MsgType
    text: str
    block_id: Optional[int] = None
    block_type: Optional[str] = None
    body_lines: int = 0
    state_from: Optional[str] = None
    state_to: Optional[str] = None

    def to_dict(self):
        d = {"seq": self.seq, "type": self.msg_type.value, "text": self.text}
        if self.block_id is not None:
            d["block_id"] = self.block_id
            d["block_type"] = self.block_type
            d["body_lines"] = self.body_lines
        if self.state_from:
            d["state_from"] = self.state_from
            d["state_to"] = self.state_to
        return d


# -- NDJSON event parser (opencode run --format json) --

@dataclass
class OpenCodeEvent:
    """Parsed opencode NDJSON event."""
    event_type: str  # step_start, step_finish, tool_use, text
    timestamp: int
    session_id: str
    part: dict
    raw: dict


def parse_ndjson_line(line: str) -> Optional[OpenCodeEvent]:
    """Parse a single NDJSON line from opencode."""
    line = line.strip()
    if not line:
        return None
    try:
        data = json.loads(line)
        return OpenCodeEvent(
            event_type=data["type"],
            timestamp=data["timestamp"],
            session_id=data["sessionID"],
            part=data["part"],
            raw=data,
        )
    except (json.JSONDecodeError, KeyError) as e:
        print(f"  [parse error: {e}]", file=sys.stderr)
        return None


# -- Translator: opencode events -> codex-ctl log messages --

class EventTranslator:
    """Converts opencode NDJSON events into codex-ctl LogMessage sequence."""

    def __init__(self):
        self.seq = 0
        self.block_id = 0
        self.messages: list[LogMessage] = []
        self.blocks: dict[int, dict] = {}  # block_id -> {header, body, type}
        self.current_step = 0
        self.total_cost = 0.0
        self.total_tokens = 0

    def next_seq(self) -> int:
        self.seq += 1
        return self.seq

    def next_block_id(self) -> int:
        self.block_id += 1
        return self.block_id

    def translate(self, event: OpenCodeEvent) -> list[LogMessage]:
        """Translate one opencode event into zero or more log messages."""
        msgs = []

        if event.event_type == "step_start":
            self.current_step += 1

        elif event.event_type == "text":
            text = event.part.get("text", "")
            if text.strip():
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.AGENT_OUTPUT,
                    text=f"\u2022 {text}",
                )
                msgs.append(msg)

        elif event.event_type == "tool_use":
            state = event.part.get("state", {})
            tool = event.part.get("tool", "")
            status = state.get("status", "")
            inp = state.get("input", {})
            output = state.get("output", "")
            title = state.get("title", "")
            metadata = state.get("metadata", {})

            # Only emit for completed/error status
            if status not in ("completed", "error"):
                return msgs

            bid = self.next_block_id()

            if tool == "write":
                filepath = inp.get("filePath", "")
                content = inp.get("content", "")
                line_count = content.count("\n")
                exists = metadata.get("exists", False)
                verb = "Edited" if exists else "Created"
                header = f"{verb} {os.path.basename(filepath)} (+{line_count} -0)"
                body = content.splitlines()
                block_type = "edited" if exists else "created"

                self.blocks[bid] = {
                    "header": header,
                    "body": body,
                    "type": block_type,
                    "filepath": filepath,
                }
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.BLOCK,
                    text=header,
                    block_id=bid,
                    block_type=block_type,
                    body_lines=len(body),
                )
                msgs.append(msg)

            elif tool == "edit":
                filepath = title or "unknown"
                header = f"Edited {filepath}"
                diff = inp.get("diff", "")
                old_str = inp.get("oldString", "")
                new_str = inp.get("newString", "")
                if old_str or new_str:
                    body = []
                    for l in old_str.splitlines():
                        body.append(f"- {l}")
                    for l in new_str.splitlines():
                        body.append(f"+ {l}")
                elif diff:
                    body = diff.splitlines()
                else:
                    body = [output] if output else []

                self.blocks[bid] = {
                    "header": header,
                    "body": body,
                    "type": "edited",
                    "filepath": filepath,
                }
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.BLOCK,
                    text=header,
                    block_id=bid,
                    block_type="edited",
                    body_lines=len(body),
                )
                msgs.append(msg)

            elif tool == "bash":
                command = inp.get("command", "")
                description = inp.get("description", "")
                exit_code = metadata.get("exit", 0)
                truncated = metadata.get("truncated", False)

                header = f"Ran {command}"
                if description:
                    header = f"Ran {description}"
                body = (output or "").splitlines()
                if truncated:
                    body.append("[output truncated]")

                self.blocks[bid] = {
                    "header": header,
                    "body": body,
                    "type": "ran",
                    "command": command,
                    "exit_code": exit_code,
                }
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.BLOCK,
                    text=header,
                    block_id=bid,
                    block_type="ran",
                    body_lines=len(body),
                )
                msgs.append(msg)

                # If exit code != 0, add status
                if exit_code != 0:
                    status_msg = LogMessage(
                        seq=self.next_seq(),
                        msg_type=MsgType.STATUS,
                        text=f"[exit code: {exit_code}]",
                    )
                    msgs.append(status_msg)

            elif tool == "read":
                filepath = title or ""
                header = f"Read {filepath}"
                body = (output or "").splitlines()

                self.blocks[bid] = {
                    "header": header,
                    "body": body,
                    "type": "read",
                    "filepath": filepath,
                }
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.BLOCK,
                    text=header,
                    block_id=bid,
                    block_type="read",
                    body_lines=len(body),
                )
                msgs.append(msg)

            elif tool == "todowrite":
                # Plan/todo updates - emit as status
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.STATUS,
                    text=f"[plan updated: {title}]",
                )
                msgs.append(msg)

            else:
                # Unknown tool - generic block
                header = f"{tool}: {title}"
                body = (output or "").splitlines() if output else []

                self.blocks[bid] = {
                    "header": header,
                    "body": body,
                    "type": tool,
                }
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.BLOCK,
                    text=header,
                    block_id=bid,
                    block_type=tool,
                    body_lines=len(body),
                )
                msgs.append(msg)

            # Error status
            if status == "error":
                err_msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.STATUS,
                    text=f"[tool error: {tool} - {output or 'unknown error'}]",
                )
                msgs.append(err_msg)

        elif event.event_type == "step_finish":
            cost = event.part.get("cost", 0)
            tokens = event.part.get("tokens", {})
            reason = event.part.get("reason", "")
            self.total_cost += cost
            self.total_tokens += tokens.get("total", 0)

            # Only emit state_change for final stop
            if reason == "stop":
                msg = LogMessage(
                    seq=self.next_seq(),
                    msg_type=MsgType.STATE_CHANGE,
                    text="",
                    state_from="working",
                    state_to="idle",
                )
                msgs.append(msg)

        self.messages.extend(msgs)
        return msgs


# -- Text mode parser (opencode run, default format) --

def parse_text_output(text: str) -> list[LogMessage]:
    """Parse opencode run default text output into log messages."""
    messages = []
    seq = 0
    block_id = 0

    lines = text.split("\n")
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue

        # Agent header: "> build . model"
        if stripped.startswith("> "):
            seq += 1
            messages.append(LogMessage(
                seq=seq,
                msg_type=MsgType.STATUS,
                text=stripped,
            ))

        # Write block: "<- Write filename"
        elif stripped.startswith("\u2190 Write ") or stripped.startswith("<- Write "):
            seq += 1
            block_id += 1
            filename = stripped.split("Write ", 1)[1] if "Write " in stripped else stripped
            messages.append(LogMessage(
                seq=seq,
                msg_type=MsgType.BLOCK,
                text=f"Created {filename}",
                block_id=block_id,
                block_type="created",
            ))

        # Edit block: "<- Edit filename"
        elif stripped.startswith("\u2190 Edit ") or stripped.startswith("<- Edit "):
            seq += 1
            block_id += 1
            filename = stripped.split("Edit ", 1)[1] if "Edit " in stripped else stripped
            messages.append(LogMessage(
                seq=seq,
                msg_type=MsgType.BLOCK,
                text=f"Edited {filename}",
                block_id=block_id,
                block_type="edited",
            ))

        # Command: "$ command"
        elif stripped.startswith("$ "):
            seq += 1
            block_id += 1
            command = stripped[2:]
            messages.append(LogMessage(
                seq=seq,
                msg_type=MsgType.BLOCK,
                text=f"Ran {command}",
                block_id=block_id,
                block_type="ran",
            ))

        # Tool output lines (indented after tool headers)
        elif stripped.startswith("Wrote file") or stripped.startswith("Applied edit"):
            # Tool confirmation - skip (noise)
            pass

        # Regular text (agent output)
        else:
            seq += 1
            messages.append(LogMessage(
                seq=seq,
                msg_type=MsgType.AGENT_OUTPUT,
                text=f"\u2022 {stripped}",
            ))

    return messages


# -- Runners --

def run_json_mode(prompt: str, cwd: str, model: Optional[str] = None) -> tuple[list[OpenCodeEvent], list[LogMessage]]:
    """Run opencode in JSON mode and translate events."""
    cmd = ["opencode", "run", "--format", "json", "--dir", cwd]
    if model:
        cmd.extend(["--model", model])
    cmd.append(prompt)

    print(f"\n{'='*60}")
    print(f"JSON MODE: {' '.join(cmd)}")
    print(f"{'='*60}\n")

    events = []
    translator = EventTranslator()

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    try:
        for line in proc.stdout:
            event = parse_ndjson_line(line)
            if event:
                events.append(event)
                msgs = translator.translate(event)
                for m in msgs:
                    # Print in codex-ctl format
                    if m.msg_type == MsgType.AGENT_OUTPUT:
                        print(m.text)
                    elif m.msg_type == MsgType.BLOCK:
                        print(f"### {m.text}")
                    elif m.msg_type == MsgType.STATUS:
                        print(m.text)
                    elif m.msg_type == MsgType.STATE_CHANGE:
                        print(f"[state: {m.state_from} \u2192 {m.state_to}]")

        proc.wait()
    except KeyboardInterrupt:
        proc.terminate()
        proc.wait()

    stderr = proc.stderr.read() if proc.stderr else ""
    if stderr.strip():
        print(f"\n[stderr: {stderr.strip()[:200]}]", file=sys.stderr)

    print(f"\n--- Summary ---")
    print(f"Events: {len(events)}")
    print(f"Messages: {len(translator.messages)}")
    print(f"Blocks: {len(translator.blocks)}")
    print(f"Cost: ${translator.total_cost:.4f}")
    print(f"Tokens: {translator.total_tokens}")
    print(f"Exit code: {proc.returncode}")

    return events, translator.messages


def run_text_mode(prompt: str, cwd: str, model: Optional[str] = None) -> tuple[str, list[LogMessage]]:
    """Run opencode in text mode (default) via PTY and parse output."""
    cmd = ["opencode", "run", "--dir", cwd]
    if model:
        cmd.extend(["--model", model])
    cmd.append(prompt)

    print(f"\n{'='*60}")
    print(f"TEXT MODE: {' '.join(cmd)}")
    print(f"{'='*60}\n")

    pid, master_fd = pty.fork()
    if pid == 0:
        os.execvp(cmd[0], cmd)
        sys.exit(1)

    raw_data = b""
    start = time.time()
    timeout = 300

    while time.time() - start < timeout:
        r, _, _ = select.select([master_fd], [], [], 1.0)
        if r:
            try:
                data = os.read(master_fd, 8192)
                if not data:
                    break
                raw_data += data
            except OSError:
                break

    try:
        os.waitpid(pid, os.WNOHANG)
    except Exception:
        pass
    os.close(master_fd)

    # Strip ANSI
    text = raw_data.decode("utf-8", errors="replace")
    clean = re.sub(r'\x1b\[[0-9;]*[a-zA-Z]', '', text)
    clean = re.sub(r'\x1b\][^\x07]*\x07', '', clean)
    clean = re.sub(r'\x1b[()][0-9A-Z]', '', clean)

    messages = parse_text_output(clean)

    print("--- Raw text output ---")
    for line in clean.split("\n"):
        if line.strip():
            print(f"  {line.rstrip()}")

    print(f"\n--- Parsed messages ---")
    for m in messages:
        if m.msg_type == MsgType.AGENT_OUTPUT:
            print(m.text)
        elif m.msg_type == MsgType.BLOCK:
            print(f"### {m.text}")
        elif m.msg_type == MsgType.STATUS:
            print(m.text)

    print(f"\n--- Summary ---")
    print(f"Messages: {len(messages)}")
    print(f"Raw bytes: {len(raw_data)}")
    print(f"Duration: {time.time()-start:.1f}s")

    return clean, messages


def run_acp_mode(prompt: str, cwd: str, model: Optional[str] = None):
    """Run opencode via ACP (JSON-RPC over stdio)."""
    print(f"\n{'='*60}")
    print(f"ACP MODE: opencode acp --cwd {cwd}")
    print(f"{'='*60}\n")

    cmd = ["opencode", "acp", "--cwd", cwd]
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    rpc_id = 0

    def send_rpc(method, params=None):
        nonlocal rpc_id
        rpc_id += 1
        msg = {
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": method,
        }
        if params:
            msg["params"] = params
        line = json.dumps(msg) + "\n"
        print(f"  >>> {method} (id={rpc_id})")
        proc.stdin.write(line)
        proc.stdin.flush()
        return rpc_id

    def read_responses(timeout_sec=30, expect_id=None):
        """Read responses until we get the expected id or timeout."""
        import select as sel
        start = time.time()
        responses = []
        notifications = []

        while time.time() - start < timeout_sec:
            r, _, _ = sel.select([proc.stdout], [], [], 0.5)
            if r:
                line = proc.stdout.readline()
                if not line:
                    break
                try:
                    data = json.loads(line.strip())
                    if "id" in data:
                        responses.append(data)
                        print(f"  <<< response id={data['id']}: {json.dumps(data.get('result', data.get('error', {})))[:200]}")
                        if expect_id and data["id"] == expect_id:
                            return responses, notifications
                    else:
                        # Notification
                        method = data.get("method", "?")
                        params = data.get("params", {})
                        notifications.append(data)

                        # Summarize notification
                        if method == "session/update":
                            update = params.get("update", {})
                            su = update.get("sessionUpdate", "")
                            if su == "tool_call":
                                tool = update.get("tool", "?")
                                status = update.get("state", {}).get("status", "?")
                                print(f"  <<< [notification] {method}: {su} tool={tool} status={status}")
                            elif su == "agent_message_chunk":
                                text = update.get("text", "")[:80]
                                print(f"  <<< [notification] {method}: {su} text={text}")
                            elif su == "tool_call_update":
                                tool = update.get("tool", "?")
                                status = update.get("state", {}).get("status", "?")
                                title = update.get("state", {}).get("title", "")
                                print(f"  <<< [notification] {method}: {su} tool={tool} status={status} title={title}")
                            else:
                                print(f"  <<< [notification] {method}: {su}")
                        elif method == "session/request_permission":
                            tool = params.get("tool", "?")
                            print(f"  <<< [notification] {method}: tool={tool} -- AUTO-APPROVING")
                            # Auto-approve (yolo mode)
                            # This is a request from agent, we need to respond
                            resp = {
                                "jsonrpc": "2.0",
                                "id": data.get("id"),
                                "result": {"approved": True},
                            }
                            proc.stdin.write(json.dumps(resp) + "\n")
                            proc.stdin.flush()
                        else:
                            print(f"  <<< [notification] {method}")
                except json.JSONDecodeError:
                    print(f"  <<< [invalid json] {line.strip()[:100]}")

        return responses, notifications

    try:
        # Step 1: Initialize
        init_id = send_rpc("initialize", {"protocolVersion": 1})
        resps, notifs = read_responses(timeout_sec=15, expect_id=init_id)

        if not resps:
            print("  [ERROR: no init response]")
            proc.terminate()
            return

        # Step 2: Create session
        session_id_rpc = send_rpc("session/new", {"cwd": cwd, "mcpServers": []})
        resps, notifs = read_responses(timeout_sec=15, expect_id=session_id_rpc)

        if not resps:
            print("  [ERROR: no session response]")
            proc.terminate()
            return

        session_result = resps[-1].get("result", {})
        session_id = session_result.get("sessionId", "")
        modes = session_result.get("modes", {})
        current_mode = modes.get("currentModeId", "")
        print(f"\n  Session: {session_id}")
        print(f"  Mode: {current_mode}")
        print(f"  Modes available: {[m['id'] for m in modes.get('availableModes', [])]}")

        # Step 3: Set mode to "build" (yolo) if not already
        if current_mode != "build":
            mode_id = send_rpc("session/set_mode", {
                "sessionId": session_id,
                "modeId": "build",
            })
            read_responses(timeout_sec=5, expect_id=mode_id)

        # Step 4: Send prompt
        print(f"\n  Sending prompt: {prompt}")
        prompt_id = send_rpc("session/prompt", {
            "sessionId": session_id,
            "parts": [{"type": "text", "text": prompt}],
        })

        # Step 5: Read all notifications until prompt completes
        resps, notifs = read_responses(timeout_sec=300, expect_id=prompt_id)

        print(f"\n--- ACP Summary ---")
        print(f"Responses: {len(resps)}")
        print(f"Notifications: {len(notifs)}")

        # Count notification types
        types = {}
        for n in notifs:
            method = n.get("method", "?")
            update = n.get("params", {}).get("update", {})
            su = update.get("sessionUpdate", method)
            types[su] = types.get(su, 0) + 1
        print(f"Notification types: {json.dumps(types, indent=2)}")

    except Exception as e:
        print(f"  [ERROR: {e}]")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


def run_compare(prompt: str, cwd: str, model: Optional[str] = None):
    """Run all modes and compare output."""
    # Create separate dirs
    json_dir = os.path.join(cwd, "json_test")
    text_dir = os.path.join(cwd, "text_test")

    for d in [json_dir, text_dir]:
        os.makedirs(d, exist_ok=True)

    events, json_msgs = run_json_mode(prompt, json_dir, model)
    raw_text, text_msgs = run_text_mode(prompt, text_dir, model)

    print(f"\n{'='*60}")
    print(f"COMPARISON")
    print(f"{'='*60}")
    print(f"JSON mode: {len(json_msgs)} messages, {len(events)} events")
    print(f"Text mode: {len(text_msgs)} messages")
    print()

    print("JSON messages:")
    for m in json_msgs:
        print(f"  [{m.msg_type.value}] {m.text[:100]}")

    print("\nText messages:")
    for m in text_msgs:
        print(f"  [{m.msg_type.value}] {m.text[:100]}")


# -- Main --

def main():
    parser = argparse.ArgumentParser(description="OpenCode debug environment")
    parser.add_argument("mode", choices=["json", "text", "acp", "compare"],
                        help="Mode: json (NDJSON), text (PTY), acp (JSON-RPC), compare (all)")
    parser.add_argument("prompt", help="Prompt to send to opencode")
    parser.add_argument("--dir", default="/tmp/opencode-debug",
                        help="Working directory (default: /tmp/opencode-debug)")
    parser.add_argument("--model", default=None,
                        help="Model in provider/model format")
    parser.add_argument("--save-events", default=None,
                        help="Save raw events to JSONL file")

    args = parser.parse_args()

    # Ensure dir exists
    os.makedirs(args.dir, exist_ok=True)

    if args.mode == "json":
        events, messages = run_json_mode(args.prompt, args.dir, args.model)
        if args.save_events:
            with open(args.save_events, "w") as f:
                for e in events:
                    f.write(json.dumps(e.raw) + "\n")
            print(f"\nSaved {len(events)} events to {args.save_events}")

    elif args.mode == "text":
        run_text_mode(args.prompt, args.dir, args.model)

    elif args.mode == "acp":
        run_acp_mode(args.prompt, args.dir, args.model)

    elif args.mode == "compare":
        run_compare(args.prompt, args.dir, args.model)


if __name__ == "__main__":
    main()
