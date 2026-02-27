# HOWTO: Using codex-ctl

## Quick start

```bash
# Spawn a session (daemon starts automatically)
ID=$(codex-ctl spawn "fix the failing tests" --cwd ~/project | jq -r .session)

# Wait for completion
codex-ctl state $ID --wait --timeout 120

# Read the log
codex-ctl log $ID --all
```

## Commands

### spawn --- start a new session

```bash
codex-ctl spawn "your prompt here" --cwd ~/project
# {"ok":true, "session":"a1b2c3d4"}
```

Options:
- `--cwd <path>` --- working directory for codex (default: daemon's cwd)
- `--gui` --- open a read-only terminal window showing live session output
- `--resume <uuid>` --- resume a previous codex session by its UUID (from `kill` response)

```bash
# Spawn with GUI debug window
codex-ctl spawn "refactor auth" --cwd ~/project --gui

# Resume a killed session
codex-ctl spawn --resume 019c8826-8134-7183-be06-6f93dd6dd5e5
```

### list --- show all active sessions

```bash
codex-ctl list
# {"sessions":[{"id":"a1b2c3d4","state":"working","cwd":"...","created_at":"...","prompt":"..."}]}
```

### state --- query or wait for session state

States: `working`, `idle`, `prompting`, `prompting_notes`, `dead`.

```bash
# Instant snapshot
codex-ctl state a1b2

# Block until idle or dead
codex-ctl state a1b2 --wait

# Block until idle or prompting (react to questions)
codex-ctl state a1b2 --wait idle,prompting

# Wait with timeout
codex-ctl state a1b2 --wait --timeout 30
```

Response examples:

```json
{"state":"idle","waited":true,"waited_sec":12.3,"timed_out":false}
{"state":"working","waited":true,"waited_sec":30.0,"timed_out":true}
{"state":"prompting","waited":true,"question_num":1,"question_total":3,"question_text":"...","options":[...],"selected":1}
{"state":"dead","exit_code":0}
```

### log --- read structured log messages

Output format: markdown-like plain text with a JSON status footer.

```bash
# Unread messages only (advances cursor)
codex-ctl log a1b2

# All messages from start
codex-ctl log a1b2 --all

# Messages with seq >= 42
codex-ctl log a1b2 --since 42

# Block until done, then return everything
codex-ctl log a1b2 --wait --timeout 120

# Stream as NDJSON until session dies
codex-ctl log a1b2 --follow
```

### act --- send keystrokes and text

```bash
# Type text and press Enter (new prompt)
codex-ctl act a1b2 "fix the failing tests in auth.rs" enter

# Navigate prompt options (down, down, Enter)
codex-ctl act a1b2 down down enter

# Interrupt current work
codex-ctl act a1b2 esc

# Interrupt, wait, then new prompt
codex-ctl act a1b2 esc wait:500 "new task" enter

# Switch to notes mode, type note, submit
codex-ctl act a1b2 tab "focus on backend only" enter

# Type the literal word "enter" (not the key)
codex-ctl act a1b2 "text:enter"
```

Reserved key names (case-insensitive): `enter`, `tab`, `esc`, `space`, `up`, `down`, `left`, `right`, `backspace`, `ctrl+c`, `ctrl+d`, `ctrl+z`, `ctrl+l`.

Special prefixes: `text:<str>` (literal text), `wait:<ms>` (pause).

### expand --- show full content of collapsed blocks

File edits, command output, and file reads are collapsed to one-line headers in the log. Use `expand` to see the full body.

```bash
codex-ctl expand a1b2 1           # single block
codex-ctl expand a1b2 1,2,3       # multiple blocks
codex-ctl expand a1b2 --all       # all blocks
```

### screen --- raw terminal dump

```bash
codex-ctl screen a1b2
# {"lines":["line1","line2",...]}   (500 rows of the virtual terminal)
```

### gui --- debug terminal window

Opens a read-only terminal window mirroring the session's PTY output in real time.

```bash
codex-ctl gui a1b2
```

### kill --- gracefully terminate a session

Sends Ctrl+C three times (so codex can output its session UUID for later resume), then SIGTERM/SIGKILL as fallback.

```bash
codex-ctl kill a1b2
# {"ok":true, "codex_session_id":"019c8826-..."}
```

The `codex_session_id` can be used with `spawn --resume` to continue the session later.

## Usage patterns

### Pattern 1: Fire and forget

Spawn, wait for completion, read the result.

```bash
ID=$(codex-ctl spawn "create a REST API for users" --cwd ~/project | jq -r .session)
codex-ctl log $ID --wait --timeout 300
```

### Pattern 2: React to prompts

Some tasks trigger interactive questions. Wait for either idle or prompting, handle each case.

```bash
ID=$(codex-ctl spawn "set up the project" --cwd ~/project | jq -r .session)

while true; do
    RESULT=$(codex-ctl state $ID --wait idle,prompting --timeout 60)
    STATE=$(echo $RESULT | jq -r .state)

    case $STATE in
        idle) break ;;
        dead) break ;;
        prompting)
            # Read the question, decide, answer
            echo $RESULT | jq .question_text,.options
            codex-ctl act $ID down enter   # select second option
            ;;
        *)
            # Timeout --- still working
            echo "Still working..."
            ;;
    esac
done

codex-ctl log $ID --all
```

### Pattern 3: Interrupt and redirect

```bash
ID=$(codex-ctl spawn "analyze all files" --cwd ~/project | jq -r .session)
sleep 10

# Interrupt
codex-ctl act $ID esc
codex-ctl state $ID --wait idle --timeout 10

# Give new instructions
codex-ctl act $ID "focus only on src/ directory" enter
codex-ctl state $ID --wait idle --timeout 120
codex-ctl log $ID --all
```

### Pattern 4: Multiple parallel sessions

```bash
ID1=$(codex-ctl spawn "implement auth module" --cwd ~/project | jq -r .session)
ID2=$(codex-ctl spawn "write tests for API" --cwd ~/project | jq -r .session)

# Wait for both
codex-ctl state $ID1 --wait --timeout 300 &
codex-ctl state $ID2 --wait --timeout 300 &
wait

# Read results
codex-ctl log $ID1 --all
codex-ctl log $ID2 --all
```

### Pattern 5: Resume after kill

```bash
ID=$(codex-ctl spawn "big refactoring task" --cwd ~/project | jq -r .session)
# ... some work happens ...

# Need to stop and continue later
KILL_RESULT=$(codex-ctl kill $ID)
UUID=$(echo $KILL_RESULT | jq -r .codex_session_id)

# Later: resume
ID2=$(codex-ctl spawn --resume $UUID | jq -r .session)
```

### Pattern 6: AI agent supervisor (Claude Code Teams)

Each Claude agent supervises a codex session via bash tool calls:

```bash
# Supervisor spawns a worker
ID=$(codex-ctl spawn "implement feature X" --cwd ~/project | jq -r .session)

# Check progress periodically
codex-ctl log $ID          # unread messages since last check

# React to prompts
RESULT=$(codex-ctl state $ID --wait idle,prompting --timeout 30)
# ... analyze question, decide answer ...
codex-ctl act $ID down enter

# Report to team lead
codex-ctl log $ID --all    # full history for context
```

## Session data on disk

```
~/.codex-ctl/
  daemon.pid
  daemon.sock
  sessions/
    a1b2c3d4/
      meta.json          # session metadata
      messages.jsonl     # structured log (collapsed blocks)
      blocks.jsonl       # full block content (for expand)
```

## Troubleshooting

**Daemon won't start**: check if an old daemon is running (`ps aux | grep codex-ctl`). Kill it and retry.

**Session stuck in working**: codex might be waiting for MCP servers to start (10-15s on first run). Use `codex-ctl screen $ID` to see the raw terminal.

**Log has too much noise**: the daemon strips UI chrome (frames, timers, help bars) automatically. If you still see noise, check the daemon version matches your built binary.

**GUI doesn't open**: set `CODEX_CTL_TERMINAL` to your terminal emulator, or check that `$DISPLAY`/`$WAYLAND_DISPLAY` is set.
