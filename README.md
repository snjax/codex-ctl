# codex-ctl

CLI daemon for programmatically controlling [OpenAI Codex](https://github.com/openai/codex) and [OpenCode](https://opencode.ai) sessions.

Codex sessions use PTY + VT100 emulation. OpenCode sessions use structured JSON events (`opencode run --format json`).

Designed for AI agents that orchestrate coding agents as subprocesses: spawn sessions, read structured logs, detect state transitions, answer prompts, send follow-ups --- all through simple shell commands returning JSON.

## Requirements

- **Rust** 1.85+ (edition 2024)
- **OpenAI Codex CLI** and/or **OpenCode** installed and available in `$PATH` (or set `CODEX_CTL_CODEX_PATH` / `CODEX_CTL_OPENCODE_PATH`)
- **Linux** (PTY via `forkpty`, Unix sockets)
- `OPENAI_API_KEY` set in environment (used by codex)

## Build

```bash
cargo build --release
```

The binary is at `target/release/codex-ctl`.

## Install

Using [just](https://github.com/casey/just):

```bash
just install     # builds release + copies to ~/.local/bin/
```

Or manually:

```bash
cargo build --release
cp target/release/codex-ctl ~/.local/bin/
```

Make sure `~/.local/bin` is in your `$PATH`.

## Uninstall

```bash
just uninstall   # removes from ~/.local/bin/
```

## Run tests

```bash
cargo test
```

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `CODEX_CTL_DIR` | `~/.codex-ctl` | Base directory for daemon socket, pid, session data |
| `CODEX_CTL_CODEX_PATH` | `which codex` | Path to the codex binary |
| `CODEX_CTL_OPENCODE_PATH` | `which opencode` | Path to the opencode binary |
| `CODEX_CTL_TERMINAL` | auto-detect | Terminal emulator for `--gui` (foot, alacritty, kitty, xterm) |

## Architecture

```
codex-ctl spawn ───────>+-------------------------------------+
codex-ctl act ... ─────>|         Session Daemon               |
codex-ctl log ... ─────>|    (one process, N sessions)         |
codex-ctl state ... ───>|                                      |
       ^                |  +---------- Session #1 ----------+  |
       |  Unix socket   |  |  PTY master/slave              |  |
       |                |  |  codex process (child)          |  |
       | ~/.codex-ctl/  |  |  vt100::Parser (500x200)       |  |
       | daemon.sock    |  |  screen -> JSONL log            |  |
       |                |  +--------------------------------+  |
       +----------------|  +---------- Session #2 ----------+  |
                        |  |  ...                            |  |
                        |  +--------------------------------+  |
                        +--------------------------------------+
```

The daemon starts automatically on first CLI invocation. Each session runs codex under a PTY with a 500-row x 200-column virtual terminal, continuously parsing output via `vt100::Parser`.

## License

MIT
