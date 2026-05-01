# Sub-Agents

## Codex (via codex-ctl)

`codex-ctl` — daemon + CLI for running OpenAI Codex sessions in background. Codex runs with `--dangerously-bypass-approvals-and-sandbox` in a PTY. Daemon auto-starts on `~/.codex-ctl/daemon.sock`.

Codex is an AI coding agent. Write prompts as specs — like Jira tickets for a thorough senior dev with zero initiative. It will implement exactly what the spec says, reliably and completely, but won't fill gaps or make creative leaps. If the spec is vague, the result will be vague.

**Critical:** if the task is research/analysis only, always start with "RESEARCH TASK - DO NOT EDIT FILES" — otherwise Codex will start implementing.

### Session Lifecycle

```
spawn  ──>  working  ──>  idle  ──>  kill (returns UUID)  ──>  resume (UUID)
              ^              │
              └── act ───────┘   (new input anytime — working or idle)
```

States: `working` → `idle` (done) / `dead` (crashed/killed).

### Commands

```bash
# --- Create / Restore ---
ID=$(codex-ctl spawn "prompt" --cwd /project | jq -r .session)
ID=$(codex-ctl spawn --resume $UUID | jq -r .session)
ID=$(codex-ctl spawn --resume $UUID "new task" | jq -r .session)

# --- Monitor ---
codex-ctl list                              # all active sessions
codex-ctl state $ID --wait --timeout 300    # block until idle/dead
codex-ctl log $ID                           # full log from start
codex-ctl next $ID                          # unread messages (advances cursor)
codex-ctl next $ID --wait                   # block until done, return unread
codex-ctl last $ID                          # last message only
codex-ctl expand $ID <block_ids...>         # expand collapsed blocks
codex-ctl screen $ID                        # raw terminal content
codex-ctl gui $ID                           # read-only GUI window

# --- Interact ---
codex-ctl act $ID "follow-up task" enter
codex-ctl act $ID esc wait:500 "new prompt" enter

# --- Terminate ---
UUID=$(codex-ctl kill $ID | jq -r .codex_session_id)  # ALWAYS capture UUID
codex-ctl killall                           # kill all sessions at once
```

- Session IDs: 8-char hex, shortenable to unique prefix (`f686` for `f6864884`)
- `log`/`next`/`last` → markdown + JSON footer; everything else → JSON
- `kill` → Ctrl+C x3 → SIGTERM → SIGKILL
- `killall` → kills all active sessions

### Session Management

**Keep sessions alive.** Idle sessions retain full context. Use `act` for follow-ups instead of spawning fresh.

**Always capture UUID on kill:** `codex-ctl kill $ID | jq -r .codex_session_id`. Without it the session is unresumable.

**Resume killed sessions:** `spawn --resume $UUID` restores all prior context. New session ID, same knowledge.

### Anti-Patterns

- **Spawn-per-subtask** — cold start every time, zero accumulated context. One session per workstream, follow up via `act`.
- **Kill + spawn instead of `act`** — throwing away context for nothing.
- **Losing UUIDs on kill** — always capture and store `codex_session_id`.

