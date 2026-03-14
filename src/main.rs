mod client;
mod daemon;
mod gui;
mod log;
mod parser;
mod protocol;
mod session;

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "codex-ctl",
    about = "Control OpenAI Codex sessions programmatically",
    long_about = "codex-ctl is a daemon+CLI tool for spawning, monitoring, and controlling \
OpenAI Codex TUI sessions via PTY emulation. Designed for AI agents that \
orchestrate Codex as a subprocess.\n\n\
The daemon starts automatically on first invocation and listens on a Unix socket \
at ~/.codex-ctl/daemon.sock (override with $CODEX_CTL_DIR).\n\n\
All commands except `log`, `next`, and `last` return JSON on stdout. \
`log`, `next`, and `last` return markdown-like text with a JSON status footer.\n\n\
Session IDs can be specified by unique prefix (like git/docker short IDs).\n\n\
Exit codes: 0 = success, 1 = error (with {\"ok\":false,\"error\":\"...\"} on stdout).",
    after_help = "ENVIRONMENT:\n\
  CODEX_CTL_DIR           Override ~/.codex-ctl base directory\n\
  CODEX_CTL_CODEX_PATH    Override codex binary path (default: `which codex`)\n\
  CODEX_CTL_TERMINAL      Override terminal emulator for --gui\n\n\
EXAMPLES:\n\
  codex-ctl spawn \"fix auth bug\" --cwd ~/project\n\
  codex-ctl state a1b2 --wait --timeout 60\n\
  codex-ctl log a1b2\n\
  codex-ctl next a1b2 --wait\n\
  codex-ctl act a1b2 down down enter\n\
  codex-ctl act a1b2 \"refactor the auth module\" enter\n\
  codex-ctl act a1b2 esc wait:500 \"new prompt\" enter\n\
  codex-ctl kill a1b2"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch a new Codex session under a PTY
    #[command(
        long_about = "Forks codex --full-auto --no-alt-screen <PROMPT> under a PTY with a 500x200 virtual \
terminal. The session runs in the background, managed by the daemon.\n\n\
With --resume <UUID>, resumes a previous codex session (from `codex resume <UUID>`).\n\
The UUID is returned by the kill command as codex_session_id.\n\n\
Returns: {\"ok\":true, \"session\":\"<8-char-id>\"}",
        after_help = "EXAMPLES:\n\
  codex-ctl spawn \"fix the failing tests\" --cwd ~/project\n\
  codex-ctl spawn \"refactor auth\" --cwd ~/project --gui\n\
  codex-ctl spawn --resume 019c8826-8134-7183-be06-6f93dd6dd5e5\n\
  codex-ctl spawn --resume 019c8826-... \"now fix tests too\"\n\
  ID=$(codex-ctl spawn \"hello\" | jq -r .session)"
    )]
    Spawn {
        /// The prompt to send to Codex (passed as `codex --full-auto <PROMPT>`).
        /// Required unless --resume is specified.
        prompt: Option<String>,

        /// Working directory for the Codex process [default: daemon's cwd]
        #[arg(long)]
        cwd: Option<String>,

        /// Open a read-only debug GUI terminal window showing live session output
        #[arg(long)]
        gui: bool,

        /// Resume a previous codex session by its UUID (from kill response's codex_session_id)
        #[arg(long)]
        resume: Option<String>,

        /// Use OpenCode backend instead of Codex
        #[arg(long)]
        opencode: bool,
    },

    /// List all active sessions
    #[command(
        long_about = "Returns all sessions tracked by the daemon, including dead ones that \
haven't been cleaned up yet.\n\n\
Returns: {\"sessions\":[{\"id\",\"state\",\"cwd\",\"created_at\",\"prompt\"},...]}",
    )]
    List,

    /// Query or wait for session state
    #[command(
        long_about = "Without --wait: returns the current state instantly.\n\
With --wait: blocks until the session reaches a target state.\n\n\
States: working, idle, prompting, prompting_notes, dead.\n\n\
When state is prompting/prompting_notes, the response includes question_num, \
question_total, question_text, options[], and selected.\n\n\
When state is dead, the response includes exit_code.",
        after_help = "EXAMPLES:\n\
  codex-ctl state a1b2                              # instant snapshot\n\
  codex-ctl state a1b2 --wait                       # block until idle or dead\n\
  codex-ctl state a1b2 --wait idle,prompting        # block until idle or prompting\n\
  codex-ctl state a1b2 --wait --timeout 30          # wait up to 30s"
    )]
    State {
        /// Session ID or unique prefix
        session: String,

        /// Block until reaching target state(s). Without a value: wait for idle or dead.
        /// With comma-separated values: wait for any of the listed states
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        wait: Option<Option<String>>,

        /// Maximum seconds to wait (requires --wait). Returns current state with
        /// timed_out:true if exceeded
        #[arg(long, requires = "wait")]
        timeout: Option<f64>,
    },

    /// Read all structured log messages from a session
    #[command(
        long_about = "Returns all log messages from session start. Output format: markdown-like \
plain text + JSON status footer.\n\
Blocks (file edits, command runs) are collapsed to one-line headers — \
use `expand` to see full content.\n\n\
With --wait: blocks until IDLE/DEAD, then returns all messages.\n\n\
With --follow: streams messages as NDJSON until the session dies.\n\n\
Use `next` instead of `log` to read only unread messages with cursor tracking.",
        after_help = "EXAMPLES:\n\
  codex-ctl log a1b2                    # all messages from start\n\
  codex-ctl log a1b2 --since 42         # messages with seq >= 42\n\
  codex-ctl log a1b2 --wait             # block until done, return all\n\
  codex-ctl log a1b2 --wait --timeout 120\n\
  codex-ctl log a1b2 --follow           # stream NDJSON"
    )]
    Log {
        /// Session ID or unique prefix
        session: String,

        /// Stream messages as NDJSON until session dies. Keeps connection open
        #[arg(long, conflicts_with = "wait")]
        follow: bool,

        /// Return messages with seq >= N
        #[arg(long)]
        since: Option<u64>,

        /// Block until session reaches IDLE or DEAD, then return all messages
        #[arg(long)]
        wait: bool,

        /// Maximum seconds to wait (requires --wait)
        #[arg(long, requires = "wait")]
        timeout: Option<f64>,
    },

    /// Read unread log messages since last check (advances cursor)
    #[command(
        long_about = "Returns only messages not yet seen, advancing the read cursor. \
Each session has one read cursor tracked by the daemon.\n\n\
Ideal for supervisor loops: call `next` periodically to get incremental updates.\n\n\
With --wait: blocks until IDLE/DEAD, then returns all unread messages.",
        after_help = "EXAMPLES:\n\
  codex-ctl next a1b2                   # unread messages since last check\n\
  codex-ctl next a1b2 --wait            # block until done, return unread\n\
  codex-ctl next a1b2 --wait --timeout 30"
    )]
    Next {
        /// Session ID or unique prefix
        session: String,

        /// Block until session reaches IDLE or DEAD, then return unread messages
        #[arg(long)]
        wait: bool,

        /// Maximum seconds to wait (requires --wait)
        #[arg(long, requires = "wait")]
        timeout: Option<f64>,
    },

    /// Get the single most recent log message
    #[command(
        long_about = "Returns the last message from the session log in text format + JSON footer. \
Useful for quick status checks without reading the full log.",
    )]
    Last {
        /// Session ID or unique prefix
        session: String,
    },

    /// Send keystrokes and text to a session
    #[command(
        long_about = "Each argument is processed left-to-right with a 30ms pause between actions.\n\n\
RESERVED KEY NAMES (case-insensitive):\n\
  enter       \\r              tab         \\t\n\
  esc         \\x1b            space       ' '\n\
  up/down/left/right           arrow keys\n\
  backspace   \\x7f\n\
  ctrl+c      \\x03            ctrl+d      \\x04\n\
  ctrl+z      \\x1a            ctrl+l      \\x0c\n\n\
SPECIAL PREFIXES:\n\
  text:<str>   Type literal text (bypass key name matching)\n\
  wait:<ms>    Pause for N milliseconds\n\n\
TEXT HANDLING:\n\
  Any argument not matching a key name or prefix is typed as text.\n\
  Text < 50 chars is sent as a single write.\n\
  Text >= 50 chars is chunked into 32-byte writes with 10ms pauses.\n\
  Newlines in text (0x0a) are sent as Ctrl+J (new line in Codex input).\n\n\
Returns: {\"ok\":true}",
        after_help = "EXAMPLES:\n\
  codex-ctl act a1b2 \"fix the bug\" enter          # type text + submit\n\
  codex-ctl act a1b2 down down enter                # navigate prompt options\n\
  codex-ctl act a1b2 esc                            # interrupt\n\
  codex-ctl act a1b2 esc wait:500 \"new task\" enter  # interrupt, wait, new prompt\n\
  codex-ctl act a1b2 tab \"my notes\" enter           # switch to notes, type, submit\n\
  codex-ctl act a1b2 \"text:enter\"                   # type literal word \"enter\"\n\
  codex-ctl act a1b2 $'line1\\nline2' enter          # multiline input (Ctrl+J)"
    )]
    Act {
        /// Session ID or unique prefix
        session: String,

        /// Sequence of actions: key names, text strings, wait:<ms>, text:<literal>
        #[arg(required = true)]
        actions: Vec<String>,
    },

    /// Show the virtual terminal screen content
    #[command(
        long_about = "By default prints cleaned-up screen content as plain text \
(trailing whitespace trimmed, blank lines collapsed, trailing blanks removed).\n\n\
--clean: also strips UI chrome (top frame, bottom help bar, timers, suggestion prompts).\n\
--raw: returns full 500-line JSON array (for programmatic use).\n\n\
Returns (default/--clean): plain text on stdout.\n\
Returns (--raw): {\"lines\":[\"line1\",\"line2\",...]}  (500 elements)",
        after_help = "EXAMPLES:\n\
  codex-ctl screen a1b2               # readable text output\n\
  codex-ctl screen a1b2 --clean       # content only, no UI chrome\n\
  codex-ctl screen a1b2 --raw         # full 500-line JSON array",
    )]
    Screen {
        /// Session ID or unique prefix
        session: String,

        /// Strip UI chrome (frame, help bar, timers, suggestion prompts)
        #[arg(long)]
        clean: bool,

        /// Return raw JSON with all 500 lines (for programmatic use)
        #[arg(long)]
        raw: bool,
    },

    /// Show full content of collapsed log blocks
    #[command(
        long_about = "The log collapses file edits, command output, and file reads into \
one-line headers. Use this command to retrieve the full body.\n\n\
Returns: {\"ok\":true, \"blocks\":[{\"id\",\"block_type\",\"header\",\"body\":[...],\"seq\"},...]}",
        after_help = "EXAMPLES:\n\
  codex-ctl expand a1b2 1                # single block\n\
  codex-ctl expand a1b2 1,2,3            # multiple blocks\n\
  codex-ctl expand a1b2 --all            # all blocks in session"
    )]
    Expand {
        /// Session ID or unique prefix
        session: String,

        /// Return all blocks in session
        #[arg(long)]
        all: bool,

        /// Block IDs: numbers (comma-separated in one arg or as separate args)
        #[arg(required_unless_present = "all")]
        block_ids: Vec<String>,
    },

    /// Open a read-only terminal window mirroring the session
    #[command(
        long_about = "Spawns a terminal emulator running `codex-ctl _gui-attach <id>`, which \
replays the current VT buffer and then streams raw PTY bytes in real time.\n\n\
The window is read-only — keystrokes in the GUI do NOT reach the session.\n\n\
Terminal search order: $CODEX_CTL_TERMINAL > $TERMINAL > foot > alacritty > kitty > xterm.\n\n\
Returns: {\"ok\":true}",
    )]
    Gui {
        /// Session ID or unique prefix
        session: String,
    },

    /// Gracefully terminate a session (Ctrl+C x3, then SIGTERM/SIGKILL fallback)
    #[command(
        long_about = "Sends Ctrl+C to the codex process three times (1s apart) so codex can \
output its session UUID for later resume. Waits up to 5s total for exit, \
scanning for the UUID in terminal output.\n\n\
Fallback: if still alive after ~3s of Ctrl+C, sends SIGTERM (500ms), then SIGKILL.\n\n\
Returns: {\"ok\":true, \"codex_session_id\":\"<uuid>\" or null}",
    )]
    Kill {
        /// Session ID or unique prefix
        session: String,
    },

    /// Kill all active sessions
    #[command(
        name = "killall",
        long_about = "Gracefully terminates all active sessions. Each session goes through \
the same kill sequence as `kill` (Ctrl+C x3 → SIGTERM → SIGKILL).\n\n\
Returns: {\"ok\":true, \"killed\":[{\"session\":\"...\", \"codex_session_id\":\"...\"},...]}",
    )]
    KillAll,

    #[command(name = "_daemon", hide = true)]
    Daemon,

    #[command(name = "_gui-attach", hide = true)]
    GuiAttach {
        session: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => {
            // Run the daemon directly
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            match rt.block_on(run_daemon()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Commands::GuiAttach { session } => {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            match rt.block_on(gui::attach::run_gui_attach(&session)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            // Client mode: auto-start daemon if needed, then send request
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            match rt.block_on(run_client(cli.command)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    let err = protocol::err_json(&e.to_string());
                    println!("{}", serde_json::to_string(&err).unwrap());
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn run_daemon() -> anyhow::Result<()> {
    let daemon = daemon::Daemon::new()?;
    daemon.run().await
}

async fn run_client(command: Commands) -> anyhow::Result<()> {
    // Try to connect; if fails, auto-start daemon
    ensure_daemon().await?;

    let request = build_request(command);

    // Check if this is a log/state with text output format
    match &request {
        protocol::Request::Log {
            follow: true, ..
        } => {
            return run_streaming_client(request).await;
        }
        _ => {}
    }

    let response = client::request(&request).await?;

    // Format output based on command type
    format_output(&request, &response);

    // Check for error
    if response.get("ok") == Some(&serde_json::Value::Bool(false)) {
        std::process::exit(1);
    }

    Ok(())
}

async fn run_streaming_client(request: protocol::Request) -> anyhow::Result<()> {
    let mut stream = client::connect().await?;
    let json = serde_json::to_string(&request)?;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    stream.write_all(json.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        // For log --follow, format each message
        let msg: serde_json::Value = serde_json::from_str(line.trim())?;
        if let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) {
            match msg_type {
                "agent_output" => {
                    if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                        println!("{text}");
                    }
                }
                "block" => {
                    if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                        println!("### {text}");
                    }
                }
                "status" => {
                    if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                        println!("{text}");
                    }
                }
                "state_change" => {
                    if let (Some(from), Some(to)) = (
                        msg.get("state_from").and_then(|s| s.as_str()),
                        msg.get("state_to").and_then(|s| s.as_str()),
                    ) {
                        println!("[state: {from} → {to}]");
                    }
                }
                _ => {
                    println!("{}", line.trim());
                }
            }
        } else {
            println!("{}", line.trim());
        }
    }

    Ok(())
}

async fn ensure_daemon() -> anyhow::Result<()> {
    // Try to connect
    if client::connect().await.is_ok() {
        return Ok(());
    }

    // Check for stale PID file
    let pid_path = client::pid_path();
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                let pid = nix::unistd::Pid::from_raw(pid);
                if nix::sys::signal::kill(pid, None).is_err() {
                    // Process not running, clean up stale files
                    let _ = std::fs::remove_file(&pid_path);
                    let _ = std::fs::remove_file(client::socket_path());
                }
            }
        }
    }

    // Ensure base dir exists
    let base_dir = client::base_dir();
    std::fs::create_dir_all(&base_dir)?;

    // Spawn daemon process
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("_daemon");

    // Propagate CODEX_CTL_DIR for test isolation
    if let Ok(dir) = std::env::var("CODEX_CTL_DIR") {
        cmd.env("CODEX_CTL_DIR", dir);
    }

    // Detach the daemon
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    use std::os::unix::process::CommandExt;
    // Create new session so daemon survives parent exit
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        });
    }

    cmd.spawn()?;

    // Wait for daemon to start accepting connections
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if client::connect().await.is_ok() {
            return Ok(());
        }
    }

    anyhow::bail!("Daemon failed to start within 2 seconds")
}

fn build_request(command: Commands) -> protocol::Request {
    match command {
        Commands::Spawn { prompt, cwd, gui, resume, opencode } => {
            if prompt.is_none() && resume.is_none() {
                eprintln!("error: <PROMPT> is required unless --resume is specified");
                std::process::exit(1);
            }
            protocol::Request::Spawn { prompt, cwd, gui, resume, opencode }
        }
        Commands::List => protocol::Request::List,
        Commands::State {
            session,
            wait,
            timeout,
        } => {
            let wait_states = wait.map(|opt| {
                opt.map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_else(|| vec!["idle".into(), "dead".into()])
            });
            protocol::Request::State {
                session,
                wait: wait_states,
                timeout,
            }
        }
        Commands::Log {
            session,
            follow,
            since,
            wait,
            timeout,
        } => protocol::Request::Log {
            session,
            follow,
            since,
            wait,
            timeout,
        },
        Commands::Next {
            session,
            wait,
            timeout,
        } => protocol::Request::Next {
            session,
            wait,
            timeout,
        },
        Commands::Last { session } => protocol::Request::Last { session },
        Commands::Act { session, actions } => protocol::Request::Act { session, actions },
        Commands::Screen { session, clean, raw } => protocol::Request::Screen { session, clean, raw },
        Commands::Expand {
            session,
            all,
            block_ids,
        } => protocol::Request::Expand {
            session,
            block_ids: if all {
                vec!["--all".into()]
            } else {
                block_ids
            },
        },
        Commands::Gui { session } => protocol::Request::Gui { session },
        Commands::Kill { session } => protocol::Request::Kill { session },
        Commands::KillAll => protocol::Request::KillAll,
        Commands::Daemon => unreachable!(),
        Commands::GuiAttach { session: _ } => unreachable!(),
    }
}

fn format_output(request: &protocol::Request, response: &serde_json::Value) {
    match request {
        protocol::Request::Log { .. } | protocol::Request::Next { .. } | protocol::Request::Last { .. } => {
            // Text format: markdown-like + JSON footer
            format_log_output(response);
        }
        protocol::Request::Screen { raw, clean, .. } => {
            format_screen_output(response, *raw, *clean);
        }
        _ => {
            // JSON output
            println!("{}", serde_json::to_string_pretty(response).unwrap());
        }
    }
}

fn format_screen_output(response: &serde_json::Value, raw: bool, clean: bool) {
    if raw {
        println!("{}", serde_json::to_string_pretty(response).unwrap());
        return;
    }

    let lines: Vec<String> = response
        .get("lines")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let filtered = crate::session::screen::filter_lines(&lines);

    let output = if clean {
        crate::session::screen::strip_ui_chrome(&filtered)
    } else {
        filtered
    };

    for line in &output {
        println!("{line}");
    }
}

fn format_log_output(response: &serde_json::Value) {
    if let Some(messages) = response.get("messages").and_then(|m| m.as_array()) {
        let log_messages: Vec<crate::log::LogMessage> = messages
            .iter()
            .filter_map(|m| serde_json::from_value(m.clone()).ok())
            .collect();

        let lines = crate::log::formatter::format_messages(&log_messages);
        for line in &lines {
            println!("{line}");
        }

        // Print footer
        let state = response
            .get("_meta")
            .and_then(|m| m.get("state"))
            .and_then(|s| s.as_str())
            .or_else(|| response.get("_state").and_then(|s| s.as_str()))
            .unwrap_or("unknown");
        let seq = log_messages.last().map(|m| m.seq).unwrap_or(0);
        let waited = response.get("_meta").is_some();
        let waited_sec = response
            .get("_meta")
            .and_then(|m| m.get("waited_sec"))
            .and_then(|s| s.as_f64());
        let timed_out = response
            .get("_meta")
            .and_then(|m| m.get("timed_out"))
            .and_then(|s| s.as_bool());

        let footer =
            crate::log::formatter::format_footer(state, seq, waited, waited_sec, timed_out);
        println!("{}", serde_json::to_string(&footer).unwrap());
    } else if response.get("ok") == Some(&serde_json::Value::Bool(false)) {
        // Error response — print as JSON
        println!("{}", serde_json::to_string_pretty(response).unwrap());
    } else {
        // Single message (last command)
        if let Some(msg_type) = response.get("type").and_then(|t| t.as_str()) {
            match msg_type {
                "agent_output" => {
                    if let Some(text) = response.get("text").and_then(|t| t.as_str()) {
                        println!("{text}");
                    }
                }
                "block" => {
                    if let Some(text) = response.get("text").and_then(|t| t.as_str()) {
                        println!("### {text}");
                    }
                }
                _ => {
                    println!("{}", serde_json::to_string_pretty(response).unwrap());
                }
            }
            // Footer for last
            let seq = response.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
            let footer = crate::log::formatter::format_footer("unknown", seq, false, None, None);
            println!("{}", serde_json::to_string(&footer).unwrap());
        } else {
            println!("{}", serde_json::to_string_pretty(response).unwrap());
        }
    }
}
