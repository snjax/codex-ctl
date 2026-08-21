use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use regex::Regex;
use tracing::{error, info, warn};

use super::Daemon;
use crate::log::reader;
use crate::protocol::{Request, err_json, ok_json};
use crate::session::Session;
use crate::session::input::{execute_actions, parse_actions, write_to_pty};
use crate::session::state::SessionState;
use crate::session::wait;

/// Handle a single (non-streaming) request.
pub async fn handle_request(
    daemon: Arc<RwLock<Daemon>>,
    request: Request,
) -> serde_json::Value {
    match request {
        Request::Ping => ok_json(serde_json::json!({"ok": true})),
        Request::List => handle_list(daemon).await,
        Request::Spawn { binary_path, prompt, cwd, gui, resume, backend, model } => {
            use crate::protocol::Backend;
            match backend {
                Backend::Opencode => {
                    handle_spawn_opencode(daemon, &binary_path, prompt.as_deref(), cwd.as_deref(), resume.as_deref(), model.as_deref()).await
                }
                Backend::Kimi => {
                    handle_spawn_kimi(daemon, &binary_path, prompt.as_deref(), cwd.as_deref(), gui, resume.as_deref()).await
                }
                Backend::Codex => {
                    handle_spawn(daemon, &binary_path, prompt.as_deref(), cwd.as_deref(), gui, resume.as_deref()).await
                }
            }
        }
        Request::State {
            session,
            wait: wait_states,
            timeout,
        } => handle_state(daemon, &session, wait_states, timeout).await,
        Request::Log {
            session,
            follow: _,
            since,
            wait: do_wait,
            timeout,
        } => handle_log(daemon, &session, since, do_wait, timeout).await,
        Request::Next {
            session,
            wait: do_wait,
            timeout,
        } => handle_next(daemon, &session, do_wait, timeout).await,
        Request::Last { session } => handle_last(daemon, &session).await,
        Request::Act { session, actions } => handle_act(daemon, &session, &actions).await,
        Request::Screen { session, .. } => handle_screen(daemon, &session).await,
        Request::Expand {
            session,
            block_ids,
        } => handle_expand(daemon, &session, &block_ids).await,
        Request::Gui { session } => handle_gui(daemon, &session).await,
        Request::Kill { session } => handle_kill(daemon, &session).await,
        Request::KillAll => handle_killall(daemon).await,
        Request::GuiAttach { .. } => {
            // Should be handled by streaming handler
            err_json("GuiAttach must use streaming mode")
        }
    }
}

/// Handle streaming requests (GuiAttach, Log --follow).
pub async fn handle_streaming(
    daemon: Arc<RwLock<Daemon>>,
    request: Request,
    mut writer: tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    match request {
        Request::GuiAttach { session } => {
            handle_gui_attach(daemon, &session, &mut writer).await?;
        }
        Request::Log {
            session,
            since,
            ..
        } => {
            handle_log_follow(daemon, &session, since, &mut writer).await?;
        }
        _ => {
            let resp = err_json("Not a streaming command");
            let json = serde_json::to_string(&resp)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
    }
    Ok(())
}

async fn handle_list(daemon: Arc<RwLock<Daemon>>) -> serde_json::Value {
    let daemon = daemon.read().await;
    let mut sessions = Vec::new();
    for (_, session) in &daemon.sessions {
        let session = session.lock().await;
        if session.state != SessionState::Dead {
            sessions.push(session.info_json());
        }
    }
    ok_json(serde_json::json!({"sessions": sessions}))
}

async fn handle_spawn(
    daemon: Arc<RwLock<Daemon>>,
    codex_path: &str,
    prompt: Option<&str>,
    cwd: Option<&str>,
    gui: bool,
    resume: Option<&str>,
) -> serde_json::Value {
    let cwd = match cwd {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    // Spawn codex under PTY
    let spawn_result = match crate::session::pty::spawn_codex(codex_path, prompt, &cwd, resume) {
        Ok(r) => r,
        Err(e) => return err_json(&format!("Failed to spawn codex: {e}")),
    };

    let display_prompt = prompt.unwrap_or("");
    let mut daemon_w = daemon.write().await;
    let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let session_dir = daemon_w.sessions_dir.join(&session_id);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return err_json(&format!("Failed to create session dir: {e}"));
    }

    let pid = spawn_result.pid;

    let (mut session, owned_fd) = match Session::new_with_owned_fd(
        spawn_result,
        display_prompt,
        &cwd,
        &session_dir,
    ) {
        Ok(s) => s,
        Err(e) => return err_json(&format!("Failed to create session: {e}")),
    };

    // Override the auto-generated ID
    session.id = session_id.clone();

    if let Err(e) = session.write_meta() {
        error!("Failed to write session meta: {e}");
    }

    let session_arc = Arc::new(tokio::sync::Mutex::new(session));
    daemon_w.sessions.insert(session_id.clone(), session_arc.clone());

    // Spawn PTY read loop
    let session_for_loop = session_arc.clone();
    let pty_broadcast = {
        let s = session_for_loop.lock().await;
        s.pty_broadcast.clone()
    };
    let daemon_for_loop = daemon.clone();
    let session_id_for_loop = session_id.clone();

    tokio::spawn(async move {
        pty_read_loop(session_for_loop, owned_fd, pty_broadcast).await;
        schedule_dead_session_cleanup(daemon_for_loop, session_id_for_loop);
    });

    // Spawn GUI if requested
    if gui {
        let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-ctl"));
        match crate::gui::spawn_gui_window(&session_id, &binary) {
            Ok(gui_pid) => {
                let mut s = session_arc.lock().await;
                s.gui_pid = Some(gui_pid);
            }
            Err(e) => {
                error!("Failed to spawn GUI: {e}");
            }
        }
    }

    info!("Spawned session {session_id}, pid {pid}");
    ok_json(serde_json::json!({"ok": true, "session": session_id}))
}

/// Kimi is PTY-based and drives with the same read-loop / cleanup / GUI plumbing
/// as codex — only the child command differs. This mirrors `handle_spawn`
/// verbatim except for the `spawn_kimi` + `Session::new_kimi` calls; keeping
/// them as separate functions makes the diff obvious and lets kimi grow
/// independent quirks later without threading a `Backend` param through the
/// codex path.
async fn handle_spawn_kimi(
    daemon: Arc<RwLock<Daemon>>,
    kimi_path: &str,
    prompt: Option<&str>,
    cwd: Option<&str>,
    gui: bool,
    resume: Option<&str>,
) -> serde_json::Value {
    let cwd = match cwd {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    let spawn_result = match crate::session::pty::spawn_kimi(kimi_path, prompt, &cwd, resume) {
        Ok(r) => r,
        Err(e) => return err_json(&format!("Failed to spawn kimi: {e}")),
    };

    let display_prompt = prompt.unwrap_or("");
    let mut daemon_w = daemon.write().await;
    let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let session_dir = daemon_w.sessions_dir.join(&session_id);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return err_json(&format!("Failed to create session dir: {e}"));
    }

    let pid = spawn_result.pid;

    let (mut session, owned_fd) = match Session::new_kimi(
        spawn_result,
        display_prompt,
        &cwd,
        &session_dir,
    ) {
        Ok(s) => s,
        Err(e) => return err_json(&format!("Failed to create session: {e}")),
    };

    session.id = session_id.clone();
    // Preseed the kimi session id when resuming so `list`/`kill` return it
    // immediately without waiting for a screen scan.
    if let Some(rid) = resume {
        session.kimi_session_id = Some(rid.to_string());
    }
    // Hold state at Working through the pre-inject settle window so
    // external `state --wait` doesn't return before the first prompt has
    // actually been submitted to kimi's TUI.
    if prompt.is_some() {
        session.awaiting_initial_prompt = true;
    }

    if let Err(e) = session.write_meta() {
        error!("Failed to write session meta: {e}");
    }

    let session_arc = Arc::new(tokio::sync::Mutex::new(session));
    daemon_w.sessions.insert(session_id.clone(), session_arc.clone());

    let session_for_loop = session_arc.clone();
    let pty_broadcast = {
        let s = session_for_loop.lock().await;
        s.pty_broadcast.clone()
    };
    let daemon_for_loop = daemon.clone();
    let session_id_for_loop = session_id.clone();

    tokio::spawn(async move {
        pty_read_loop(session_for_loop, owned_fd, pty_broadcast).await;
        schedule_dead_session_cleanup(daemon_for_loop, session_id_for_loop);
    });

    // Kimi refuses positional prompts on the CLI (they parse as
    // subcommand names). Type the initial prompt into the PTY once kimi's
    // TUI settles. We watch `last_esc_seen` for a quiet window rather
    // than `wait_for_state(Idle)` because we intentionally suppress the
    // pre-inject Idle so external `state --wait` doesn't fire early.
    if let Some(prompt_text) = prompt {
        let session_for_prompt = session_arc.clone();
        let prompt_owned = prompt_text.to_string();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let master_fd = loop {
                if tokio::time::Instant::now() > deadline {
                    error!("Timed out waiting for kimi TUI to settle for prompt injection");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                let s = session_for_prompt.lock().await;
                if s.state == SessionState::Dead {
                    return;
                }
                let quiet_for = std::time::Instant::now().duration_since(s.last_esc_seen);
                // ≥1.5s of PTY silence + trust prompt already dismissed
                // (or absent) means kimi's TUI is done painting and the
                // input line is receptive.
                // 3s of PTY silence gives kimi's TUI time to fully settle
                // after startup splash + trust-prompt dismissal. 1.5s
                // sometimes injects before kimi's input handler is ready
                // and Enter is dropped.
                if s.has_seen_esc && quiet_for >= Duration::from_millis(3000) {
                    info!("kimi: settling done ({:?} silent), injecting prompt ({} bytes)", quiet_for, prompt_owned.len());
                    break s.master_fd;
                }
            };
            if let Err(e) = write_to_pty(master_fd, prompt_owned.as_bytes()) {
                error!("Failed to type initial prompt into kimi PTY: {e}");
                return;
            }
            // Small delay so kimi's TUI processes the paste of prompt
            // chars before we send Enter — without this, Enter can race
            // ahead of the last few chars and get dropped.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = write_to_pty(master_fd, b"\r");
            // Give kimi enough time to actually start processing the
            // submitted message so the next Idle transition reflects
            // real work-done, not the pre-inject settle.
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut s = session_for_prompt.lock().await;
            s.awaiting_initial_prompt = false;
        });
    }

    if gui {
        let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-ctl"));
        match crate::gui::spawn_gui_window(&session_id, &binary) {
            Ok(gui_pid) => {
                let mut s = session_arc.lock().await;
                s.gui_pid = Some(gui_pid);
            }
            Err(e) => {
                error!("Failed to spawn GUI: {e}");
            }
        }
    }

    info!("Spawned kimi session {session_id}, pid {pid}");
    ok_json(serde_json::json!({"ok": true, "session": session_id}))
}

async fn handle_spawn_opencode(
    daemon: Arc<RwLock<Daemon>>,
    binary_path: &str,
    prompt: Option<&str>,
    cwd: Option<&str>,
    resume: Option<&str>,
    model: Option<&str>,
) -> serde_json::Value {
    let prompt = match prompt {
        Some(p) => p,
        None => return err_json("Prompt is required for opencode spawn"),
    };

    let cwd = match cwd {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    // Bring up the persistent server before any state mutation. If it can't
    // start we'd rather fail the spawn cleanly than leave a half-registered
    // session behind.
    let server_url = {
        let d = daemon.read().await;
        match d.ensure_opencode_server(binary_path).await {
            Ok(u) => u,
            Err(e) => return err_json(&format!("Failed to bring up opencode server: {e}")),
        }
    };

    let mut daemon_w = daemon.write().await;
    let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let session_dir = daemon_w.sessions_dir.join(&session_id);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return err_json(&format!("Failed to create session dir: {e}"));
    }

    let session = match Session::new_opencode(binary_path, prompt, &cwd, &session_dir) {
        Ok(s) => s,
        Err(e) => return err_json(&format!("Failed to create session: {e}")),
    };

    let session_arc = Arc::new(tokio::sync::Mutex::new(session));
    {
        let mut s = session_arc.lock().await;
        s.id = session_id.clone();
        // Remember the upstream opencode session id so this first run
        // launches as `opencode run --continue --session <sid>` and every
        // subsequent `act` continues the same upstream session.
        if let Some(sid) = resume {
            s.opencode_session_id = Some(sid.to_string());
        }
        // Persist the model so both this first run and every follow-up `act`
        // continuation pass the same `--model`.
        s.model = model.map(|m| m.to_string());
        if let Err(e) = s.write_meta() {
            error!("Failed to write session meta: {e}");
        }
    }

    daemon_w.sessions.insert(session_id.clone(), session_arc.clone());
    drop(daemon_w);

    // Spawn opencode subprocess in background
    let session_for_loop = session_arc.clone();
    let cwd_clone = cwd.clone();
    let prompt_owned = prompt.to_string();
    let binary_owned = binary_path.to_string();
    let server_owned = server_url.clone();
    let resume_owned = resume.map(|s| s.to_string());
    let model_owned = model.map(|s| s.to_string());

    tokio::spawn(async move {
        opencode_run_loop(
            session_for_loop,
            &binary_owned,
            &server_owned,
            &prompt_owned,
            &cwd_clone,
            resume_owned.as_deref(),
            model_owned.as_deref(),
        )
        .await;
        // Don't schedule cleanup — opencode sessions stay idle for act
    });

    info!(
        "Spawned opencode session {session_id}{}",
        resume.map(|s| format!(" (resuming {s})")).unwrap_or_default()
    );
    ok_json(serde_json::json!({"ok": true, "session": session_id}))
}

/// Run a single `opencode run` subprocess and consume its events.
/// When done, transitions the session to Idle.
async fn opencode_run_loop(
    session: Arc<tokio::sync::Mutex<Session>>,
    binary: &str,
    server_url: &str,
    prompt: &str,
    cwd: &std::path::Path,
    resume_sid: Option<&str>,
    model: Option<&str>,
) {
    let mut child = match crate::session::opencode::spawn_opencode_run(binary, server_url, prompt, cwd, resume_sid, model) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to spawn opencode: {e}");
            let mut s = session.lock().await;
            s.mark_dead(Some(1));
            return;
        }
    };

    // Consume NDJSON events, writing to session log
    let result = crate::session::opencode::consume_events(&mut child, &session).await;

    match result {
        Ok((oc_sid, exit_code)) => {
            let mut s = session.lock().await;
            if let Some(sid) = oc_sid {
                s.opencode_session_id = Some(sid);
            }

            // Transition to idle (not dead — opencode sessions can continue)
            let old_state = s.state.to_string();
            s.state = SessionState::Idle;
            let _ = s.state_tx.send(SessionState::Idle);
            let msg = crate::log::LogMessage::state_change(s.next_seq, &old_state, "idle");
            s.next_seq += 1;
            let _ = s.log_writer.append_message(&msg);
            s.exit_code = exit_code;

            info!(
                "OpenCode session {} completed (exit={:?}, oc_sid={:?})",
                s.id, exit_code, s.opencode_session_id
            );
        }
        Err(e) => {
            error!("OpenCode event consumption failed: {e}");
            let mut s = session.lock().await;
            s.mark_dead(Some(1));
        }
    }
}

/// PTY read loop: reads bytes from master fd, feeds to session, broadcasts.
async fn pty_read_loop(
    session: Arc<tokio::sync::Mutex<Session>>,
    owned_fd: std::os::fd::OwnedFd,
    pty_broadcast: tokio::sync::broadcast::Sender<Bytes>,
) {
    let async_fd = match tokio::io::unix::AsyncFd::new(owned_fd) {
        Ok(fd) => fd,
        Err(e) => {
            error!("Failed to create AsyncFd: {e}");
            return;
        }
    };

    let mut buf = [0u8; 8192];
    let mut tick_interval = tokio::time::interval(Duration::from_millis(50));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            ready = async_fd.readable() => {
                match ready {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| {
                            let fd = inner.as_raw_fd();
                            // Use nix::unistd::read for raw fd reading
                            match nix::unistd::read(fd, &mut buf) {
                                Ok(0) => Ok(0),
                                Ok(n) => Ok(n),
                                Err(nix::errno::Errno::EAGAIN) => {
                                    Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                                }
                                Err(nix::errno::Errno::EIO) => {
                                    // EIO means child exited
                                    Ok(0)
                                }
                                Err(e) => {
                                    Err(std::io::Error::new(std::io::ErrorKind::Other, e))
                                }
                            }
                        }) {
                            Ok(Ok(0)) => {
                                // EOF — child exited
                                info!("PTY EOF");
                                reap_child(&session).await;
                                return;
                            }
                            Ok(Ok(n)) => {
                                let data = &buf[..n];
                                // Broadcast raw bytes for GUI
                                let _ = pty_broadcast.send(Bytes::copy_from_slice(data));
                                // Feed session
                                let mut s = session.lock().await;
                                let needs_dismiss = s.on_pty_data(data);
                                if needs_dismiss {
                                    // Codex highlights "Yes, continue" by
                                    // default, so a single Enter accepts.
                                    // Kimi highlights "Don't trust" (which
                                    // exits kimi), so we need Up-arrow
                                    // first to move to "Trust this folder".
                                    let master_fd = s.master_fd;
                                    let bytes: &[u8] = if s.is_kimi() {
                                        b"\x1b[A\r"
                                    } else {
                                        b"\r"
                                    };
                                    drop(s);
                                    info!("Auto-dismissing trust directory prompt");
                                    let _ = write_to_pty(master_fd, bytes);
                                }
                            }
                            Ok(Err(e)) => {
                                error!("PTY read error: {e}");
                                reap_child(&session).await;
                                return;
                            }
                            Err(_would_block) => {
                                // Spurious readiness, continue
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        error!("AsyncFd readiness error: {e}");
                        reap_child(&session).await;
                        return;
                    }
                }
            }
            _ = tick_interval.tick() => {
                let mut s = session.lock().await;
                s.tick();
            }
        }
    }
}

async fn reap_child(session: &Arc<tokio::sync::Mutex<Session>>) {
    let mut s = session.lock().await;
    let pid = s.pid;
    match nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(nix::sys::wait::WaitStatus::Exited(_, code)) => {
            s.mark_dead(Some(code));
            info!("Session {} child exited with code {code}", s.id);
        }
        Ok(nix::sys::wait::WaitStatus::Signaled(_, sig, _)) => {
            s.mark_dead(Some(128 + sig as i32));
            info!("Session {} child killed by signal {sig}", s.id);
        }
        _ => {
            s.mark_dead(None);
            info!("Session {} child exited (unknown status)", s.id);
        }
    }
}

/// Schedule removal of a dead session from the daemon map and disk.
/// Waits a few seconds so in-flight requests (state --wait, log --wait) can complete.
fn schedule_dead_session_cleanup(daemon: Arc<RwLock<Daemon>>, session_id: String) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut d = daemon.write().await;
        if let Some(session) = d.sessions.get(&session_id) {
            let s = session.lock().await;
            if s.state == SessionState::Dead {
                let session_dir = s.session_dir.clone();
                drop(s);
                d.sessions.remove(&session_id);
                if session_dir.exists() {
                    let _ = std::fs::remove_dir_all(&session_dir);
                }
                info!("Cleaned up dead session {session_id}");
            }
        }
    });
}

async fn handle_state(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    wait_states: Option<Vec<String>>,
    timeout: Option<f64>,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    if let Some(wait_states) = wait_states {
        // Blocking wait
        let target_states: Vec<SessionState> = wait_states
            .iter()
            .filter_map(|s| SessionState::from_str_loose(s))
            .collect();

        let target = if target_states.is_empty() {
            vec![SessionState::Idle, SessionState::Dead, SessionState::Prompting, SessionState::PromptingNotes]
        } else {
            target_states
        };

        let timeout_dur = timeout.map(|s| Duration::from_secs_f64(s));
        let state_rx = {
            let s = session.lock().await;
            s.state_rx.clone()
        };

        let result = wait::wait_for_state(state_rx, &target, timeout_dur).await;

        let mut resp = serde_json::json!({
            "state": result.state.to_string(),
            "waited": result.waited,
            "waited_sec": result.waited_sec,
            "timed_out": result.timed_out,
        });

        // Add prompt info if prompting
        let s = session.lock().await;
        if s.state == SessionState::Prompting || s.state == SessionState::PromptingNotes {
            let snapshot = s.screen_lines_filtered();
            if let Some(info) = crate::parser::prompt::parse_prompt(&snapshot) {
                resp["question_num"] = serde_json::json!(info.question_num);
                resp["question_total"] = serde_json::json!(info.question_total);
                resp["question_text"] = serde_json::json!(info.question_text);
                resp["options"] = serde_json::json!(info.options);
                resp["selected"] = serde_json::json!(info.selected);
            }
        }

        if s.state == SessionState::Dead {
            resp["exit_code"] = serde_json::json!(s.exit_code);
        }

        ok_json(resp)
    } else {
        // Instant snapshot
        let s = session.lock().await;
        let mut resp = serde_json::json!({
            "state": s.state.to_string(),
            "waited": false,
        });

        if s.state == SessionState::Prompting || s.state == SessionState::PromptingNotes {
            let snapshot = s.screen_lines_filtered();
            if let Some(info) = crate::parser::prompt::parse_prompt(&snapshot) {
                resp["question_num"] = serde_json::json!(info.question_num);
                resp["question_total"] = serde_json::json!(info.question_total);
                resp["question_text"] = serde_json::json!(info.question_text);
                resp["options"] = serde_json::json!(info.options);
                resp["selected"] = serde_json::json!(info.selected);
            }
        }

        if s.state == SessionState::Dead {
            resp["exit_code"] = serde_json::json!(s.exit_code);
        }

        ok_json(resp)
    }
}

async fn handle_log(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    since: Option<u64>,
    do_wait: bool,
    timeout: Option<f64>,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    if do_wait {
        let timeout_dur = timeout.map(|s| Duration::from_secs_f64(s));
        let state_rx = {
            let s = session.lock().await;
            s.state_rx.clone()
        };

        let wait_result = wait::wait_for_state(
            state_rx,
            &[SessionState::Idle, SessionState::Dead, SessionState::Prompting, SessionState::PromptingNotes],
            timeout_dur,
        )
        .await;

        let mut s = session.lock().await;
        // Force pending BufWriter content to disk before reading. Codex
        // sessions also flush from `tick()` every ~50ms, but opencode
        // sessions have no tick — without this, messages.jsonl can stay
        // empty for the entire run.
        let _ = s.log_writer.flush_all();
        let messages_path = s.log_writer.messages_path().to_path_buf();
        let cursor = since.unwrap_or(0);
        drop(s);

        let messages = match reader::read_since(&messages_path, cursor) {
            Ok(m) => m,
            Err(e) => return err_json(&format!("Failed to read log: {e}")),
        };

        ok_json(serde_json::json!({
            "messages": messages,
            "_meta": {
                "state": wait_result.state.to_string(),
                "waited_sec": wait_result.waited_sec,
                "timed_out": wait_result.timed_out,
            }
        }))
    } else {
        let mut s = session.lock().await;
        let _ = s.log_writer.flush_all();
        let messages_path = s.log_writer.messages_path().to_path_buf();
        let cursor = since.unwrap_or(0);
        drop(s);

        let messages = match reader::read_since(&messages_path, cursor) {
            Ok(m) => m,
            Err(e) => return err_json(&format!("Failed to read log: {e}")),
        };

        let s = session.lock().await;
        ok_json(serde_json::json!({
            "messages": messages,
            "_state": s.state.to_string(),
        }))
    }
}

async fn handle_next(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    do_wait: bool,
    timeout: Option<f64>,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    if do_wait {
        let timeout_dur = timeout.map(|s| Duration::from_secs_f64(s));
        let state_rx = {
            let s = session.lock().await;
            s.state_rx.clone()
        };

        let wait_result = wait::wait_for_state(
            state_rx,
            &[SessionState::Idle, SessionState::Dead, SessionState::Prompting, SessionState::PromptingNotes],
            timeout_dur,
        )
        .await;

        let mut s = session.lock().await;
        let _ = s.log_writer.flush_all();
        let messages_path = s.log_writer.messages_path().to_path_buf();
        let cursor = s.read_cursor;
        drop(s);

        let messages = match reader::read_since(&messages_path, cursor) {
            Ok(m) => m,
            Err(e) => return err_json(&format!("Failed to read log: {e}")),
        };

        let mut s = session.lock().await;
        if let Some(last) = messages.last() {
            s.read_cursor = last.seq + 1;
        }

        ok_json(serde_json::json!({
            "messages": messages,
            "_meta": {
                "state": wait_result.state.to_string(),
                "waited_sec": wait_result.waited_sec,
                "timed_out": wait_result.timed_out,
            }
        }))
    } else {
        let mut s = session.lock().await;
        let _ = s.log_writer.flush_all();
        let messages_path = s.log_writer.messages_path().to_path_buf();
        let cursor = s.read_cursor;

        let messages = match reader::read_since(&messages_path, cursor) {
            Ok(m) => m,
            Err(e) => return err_json(&format!("Failed to read log: {e}")),
        };

        if let Some(last) = messages.last() {
            s.read_cursor = last.seq + 1;
        }

        ok_json(serde_json::json!({
            "messages": messages,
            "_state": s.state.to_string(),
        }))
    }
}

async fn handle_last(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    let mut s = session.lock().await;
    let _ = s.log_writer.flush_all();
    let messages_path = s.log_writer.messages_path().to_path_buf();
    drop(s);

    let messages = match reader::read_all(&messages_path) {
        Ok(m) => m,
        Err(e) => return err_json(&format!("Failed to read log: {e}")),
    };

    match messages.last() {
        Some(msg) => ok_json(serde_json::json!(msg)),
        None => err_json("No messages"),
    }
}

async fn handle_act(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    actions: &[String],
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    let (is_opencode, state) = {
        let s = session.lock().await;
        (s.is_opencode(), s.state.clone())
    };

    if state == SessionState::Dead {
        return err_json("Session is dead");
    }

    if is_opencode {
        return handle_act_opencode(daemon, session, actions).await;
    }

    // Codex: send keystrokes via PTY
    let master_fd = {
        let s = session.lock().await;
        s.master_fd
    };

    let action_strs: Vec<&str> = actions.iter().map(|s| s.as_str()).collect();
    let parsed = parse_actions(&action_strs);

    match execute_actions(master_fd, &parsed).await {
        Ok(()) => ok_json(serde_json::json!({"ok": true})),
        Err(e) => err_json(&format!("Failed to execute actions: {e}")),
    }
}

/// Handle `act` for opencode sessions: extract prompt from actions,
/// wait for idle if working, then spawn a continuation subprocess.
async fn handle_act_opencode(
    daemon: Arc<RwLock<Daemon>>,
    session: Arc<tokio::sync::Mutex<Session>>,
    actions: &[String],
) -> serde_json::Value {
    // For opencode, actions are treated as a prompt string
    // (ignore keystrokes like "enter", "esc" — they don't apply)
    let prompt: String = actions
        .iter()
        .filter(|a| {
            let lower = a.to_lowercase();
            !matches!(
                lower.as_str(),
                "enter" | "esc" | "tab" | "up" | "down" | "left" | "right"
                    | "backspace" | "space" | "ctrl+c" | "ctrl+d"
            ) && !lower.starts_with("wait:")
        })
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");

    if prompt.trim().is_empty() {
        return err_json("No prompt text found in actions (opencode mode ignores keystrokes)");
    }

    // Wait for session to be idle if it's currently working
    {
        let s = session.lock().await;
        if s.state == SessionState::Working {
            let state_rx = s.state_rx.clone();
            drop(s);
            let _ = wait::wait_for_state(
                state_rx,
                &[SessionState::Idle, SessionState::Dead, SessionState::Prompting, SessionState::PromptingNotes],
                Some(Duration::from_secs(300)),
            )
            .await;

            let s = session.lock().await;
            if s.state == SessionState::Dead {
                return err_json("Session died while waiting");
            }
        }
    }

    // Set state to Working
    {
        let mut s = session.lock().await;
        let old_state = s.state.to_string();
        s.state = SessionState::Working;
        let _ = s.state_tx.send(SessionState::Working);
        let msg = crate::log::LogMessage::state_change(s.next_seq, &old_state, "working");
        s.next_seq += 1;
        let _ = s.log_writer.append_message(&msg);
    }

    // Get opencode session ID, cwd, binary path, and model for continuation
    let (oc_sid, cwd, binary, model) = {
        let s = session.lock().await;
        (
            s.opencode_session_id.clone(),
            s.cwd.clone(),
            s.opencode_binary.clone(),
            s.model.clone(),
        )
    };

    let binary = match binary {
        Some(b) => b,
        None => return err_json("Opencode session has no recorded binary path (was it spawned before this fix?)"),
    };

    let server_url = {
        let d = daemon.read().await;
        match d.ensure_opencode_server(&binary).await {
            Ok(u) => u,
            Err(e) => return err_json(&format!("Failed to bring up opencode server: {e}")),
        }
    };

    // Spawn continuation in background
    let session_for_loop = session.clone();

    tokio::spawn(async move {
        opencode_run_loop(
            session_for_loop,
            &binary,
            &server_url,
            &prompt,
            &cwd,
            oc_sid.as_deref(),
            model.as_deref(),
        )
        .await;
    });

    ok_json(serde_json::json!({"ok": true}))
}

async fn handle_screen(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    let mut s = session.lock().await;
    // OpenCode has no TUI, so the dummy 1x1 VT parser would return garbage.
    // Render the equivalent "what the agent is showing right now" from the
    // structured JSON log instead — same formatter as the CLI's `log`
    // markdown render.
    if s.is_opencode() {
        let _ = s.log_writer.flush_all();
        let messages_path = s.log_writer.messages_path().to_path_buf();
        drop(s);

        let messages = match reader::read_all(&messages_path) {
            Ok(m) => m,
            Err(e) => return err_json(&format!("Failed to read log: {e}")),
        };
        let lines = crate::log::formatter::format_messages(&messages);
        return ok_json(serde_json::json!({"lines": lines}));
    }

    let lines = s.screen_lines();
    ok_json(serde_json::json!({"lines": lines}))
}

async fn handle_expand(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    block_ids: &[String],
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    // Snapshot the metadata under the session lock, plus flush the body
    // BufWriter so the offsets we're about to read are all backed by file
    // bytes. Then drop the lock before doing any disk reads — we don't want
    // to hold the session mutex while a multi-MB body is pread'd from disk.
    let (selected, bodies_path) = {
        let mut s = session.lock().await;
        let _ = s.log_writer.flush_all();
        let bodies_path = s.log_writer.bodies_path().to_path_buf();

        let mut selected: Vec<crate::session::BlockMeta> = Vec::new();
        if block_ids.len() == 1 && block_ids[0] == "--all" {
            selected.extend(s.blocks.values().cloned());
            selected.sort_by_key(|m| m.id);
        } else {
            for id_str in block_ids {
                for part in id_str.split(',') {
                    let part = part.trim();
                    if let Ok(id) = part.parse::<u64>() {
                        if let Some(meta) = s.blocks.get(&id) {
                            selected.push(meta.clone());
                        }
                    }
                }
            }
        }
        (selected, bodies_path)
    };

    // Open bodies.bin once for the whole expand, then seek+read per block.
    // One open() + N pread-equivalents instead of N opens — cheap even for
    // `--all` on a multi-thousand-block session.
    let mut file = match std::fs::File::open(&bodies_path) {
        Ok(f) => f,
        Err(e) => return err_json(&format!("Failed to open bodies.bin: {e}")),
    };

    use std::io::{Read, Seek, SeekFrom};
    let mut result_blocks = Vec::new();
    for meta in &selected {
        let mut buf = vec![0u8; meta.body_len as usize];
        if meta.body_len > 0 {
            if let Err(e) = file.seek(SeekFrom::Start(meta.body_offset)) {
                tracing::error!("Failed to seek in bodies.bin for block {}: {e}", meta.id);
                continue;
            }
            if let Err(e) = file.read_exact(&mut buf) {
                tracing::error!("Failed to read body for block {}: {e}", meta.id);
                continue;
            }
        }
        let body = crate::session::decode_body(&buf);
        result_blocks.push(serde_json::json!({
            "id": meta.id,
            "block_type": meta.block_type,
            "header": meta.header,
            "body": body,
            "seq": meta.seq,
        }));
    }

    ok_json(serde_json::json!({"ok": true, "blocks": result_blocks}))
}

async fn handle_gui(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-ctl"));
    let session_id = {
        let s = session.lock().await;
        s.id.clone()
    };

    match crate::gui::spawn_gui_window(&session_id, &binary) {
        Ok(gui_pid) => {
            let mut s = session.lock().await;
            s.gui_pid = Some(gui_pid);
            ok_json(serde_json::json!({"ok": true}))
        }
        Err(e) => err_json(&format!("Failed to spawn GUI: {e}")),
    }
}

/// Scan screen lines for codex session UUID in "codex resume <UUID>" pattern.
fn scan_for_codex_session_id(lines: &[String]) -> Option<String> {
    let re = Regex::new(r"codex resume ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})").unwrap();
    for line in lines {
        if let Some(caps) = re.captures(line) {
            return Some(caps[1].to_string());
        }
    }
    None
}

/// Refresh both codex and kimi session-id caches on the given session in one
/// shot. Kept as a helper because every kill-loop return site needs the
/// same pair of scans and inlining them all triples the boilerplate.
fn capture_pty_session_ids(s: &mut Session) {
    if s.codex_session_id.is_none() {
        let lines = s.screen_lines();
        if let Some(uuid) = scan_for_codex_session_id(&lines) {
            info!("Captured codex session ID: {uuid}");
            s.codex_session_id = Some(uuid);
        }
    }
    if s.is_kimi() && s.kimi_session_id.is_none() {
        if let Some(uuid) = scan_for_kimi_session_id(s.created_at) {
            info!("Captured kimi session ID: {uuid}");
            s.kimi_session_id = Some(uuid);
        }
    }
}

/// Find the kimi session UUID created since `created_at` by walking
/// `~/.kimi-code/sessions/wd_*/session_<UUID>/` and picking the newest
/// directory whose mtime falls at or after the session's start.
///
/// Kimi doesn't announce its session id in the TUI (unlike codex, which
/// prints `codex resume <uuid>` on `ctrl+c`), so we recover it from disk
/// state. The bound on `created_at` prevents us from claiming an
/// unrelated older session dir when the user happens to spawn kimi with
/// no new session having been persisted yet.
fn scan_for_kimi_session_id(created_at: chrono::DateTime<chrono::Utc>) -> Option<String> {
    use std::time::{Duration, SystemTime};
    let home = std::env::var("HOME").ok()?;
    let sessions_root = std::path::Path::new(&home).join(".kimi-code/sessions");
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(created_at.timestamp().max(0) as u64);

    let mut best: Option<(SystemTime, String)> = None;
    let wd_iter = std::fs::read_dir(&sessions_root).ok()?;
    for wd_entry in wd_iter.flatten() {
        let wd_name = wd_entry.file_name();
        let wd_name = wd_name.to_string_lossy();
        if !wd_name.starts_with("wd_") {
            continue;
        }
        let sess_iter = match std::fs::read_dir(wd_entry.path()) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for sess_entry in sess_iter.flatten() {
            let name = sess_entry.file_name();
            let name = name.to_string_lossy().into_owned();
            // Kimi's own resume expects the full `session_<UUID>` form as
            // passed to `-S`, not the bare UUID — matches the on-disk
            // directory name.
            if !name.starts_with("session_") {
                continue;
            }
            let meta = match sess_entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                continue;
            }
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, name));
            }
        }
    }
    best.map(|(_, u)| u)
}

/// Check if process has exited (non-blocking). Returns Some(exit_code) or None if still alive.
fn try_reap(pid: nix::unistd::Pid) -> Option<Option<i32>> {
    match nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(nix::sys::wait::WaitStatus::Exited(_, code)) => Some(Some(code)),
        Ok(nix::sys::wait::WaitStatus::Signaled(_, sig, _)) => Some(Some(128 + sig as i32)),
        Ok(nix::sys::wait::WaitStatus::StillAlive) => None,
        _ => None,
    }
}

async fn handle_kill(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
) -> serde_json::Value {
    let session = {
        let d = daemon.read().await;
        match d.resolve_session(session_id) {
            Ok(s) => s,
            Err(e) => return err_json(&e.to_string()),
        }
    };

    let is_opencode = {
        let s = session.lock().await;
        s.is_opencode()
    };

    let result = if is_opencode {
        kill_opencode_session(&session).await
    } else {
        kill_session(&session).await
    };

    // Schedule cleanup (removes from daemon map + deletes session dir)
    schedule_dead_session_cleanup(daemon, session_id.to_string());

    result
}

/// Perform the actual kill sequence, returning the JSON response.
async fn kill_session(
    session: &Arc<tokio::sync::Mutex<Session>>,
) -> serde_json::Value {
    // Check if already dead
    {
        let s = session.lock().await;
        if s.state == SessionState::Dead {
            return ok_json(serde_json::json!({
                "ok": true,
                "codex_session_id": s.codex_session_id,
                "kimi_session_id": s.kimi_session_id,
            }));
        }
    }

    // Phase 1: Graceful Ctrl+C (3 times, 1s apart)
    for i in 0..3 {
        {
            let s = session.lock().await;
            let master_fd = s.master_fd;
            if let Err(e) = write_to_pty(master_fd, b"\x03") {
                warn!("Failed to write Ctrl+C to PTY: {e}");
            }
        }

        if i < 2 {
            // Wait 1s between Ctrl+C signals, polling for UUID and exit
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(100)).await;

                let mut s = session.lock().await;
                capture_pty_session_ids(&mut s);

                // Check if process exited
                if let Some(code) = try_reap(s.pid) {
                    s.mark_dead(code);
                    return ok_json(serde_json::json!({
                        "ok": true,
                        "codex_session_id": s.codex_session_id,
                        "kimi_session_id": s.kimi_session_id,
                    }));
                }
            }
        }
    }

    // Phase 2: Wait up to 2 more seconds (total ~5s) polling for UUID and exit
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut s = session.lock().await;
        capture_pty_session_ids(&mut s);

        // Check if process exited
        if let Some(code) = try_reap(s.pid) {
            s.mark_dead(code);
            return ok_json(serde_json::json!({
                "ok": true,
                "codex_session_id": s.codex_session_id,
                "kimi_session_id": s.kimi_session_id,
            }));
        }
    }

    // Phase 3: SIGTERM fallback
    {
        let s = session.lock().await;
        warn!("Codex didn't exit after Ctrl+C, sending SIGTERM");
        let _ = nix::sys::signal::kill(s.pid, nix::sys::signal::Signal::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let mut s = session.lock().await;
        if let Some(code) = try_reap(s.pid) {
            s.mark_dead(code);
            capture_pty_session_ids(&mut s);
            return ok_json(serde_json::json!({
                "ok": true,
                "codex_session_id": s.codex_session_id,
                "kimi_session_id": s.kimi_session_id,
            }));
        }
    }

    // Phase 4: SIGKILL
    {
        let s = session.lock().await;
        warn!("Codex didn't exit after SIGTERM, sending SIGKILL");
        let _ = nix::sys::signal::kill(s.pid, nix::sys::signal::Signal::SIGKILL);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut s = session.lock().await;
    let exit_code = try_reap(s.pid).unwrap_or(None);
    s.mark_dead(exit_code);

    if s.is_kimi() && s.kimi_session_id.is_none() {
        s.kimi_session_id = scan_for_kimi_session_id(s.created_at);
    }
    ok_json(serde_json::json!({
        "ok": true,
        "codex_session_id": s.codex_session_id,
        "kimi_session_id": s.kimi_session_id,
    }))
}

/// Kill an opencode session: just mark dead. The subprocess (if running) will
/// be dropped when the session is cleaned up.
async fn kill_opencode_session(
    session: &Arc<tokio::sync::Mutex<Session>>,
) -> serde_json::Value {
    let mut s = session.lock().await;
    if s.state == SessionState::Dead {
        return ok_json(serde_json::json!({
            "ok": true,
            "codex_session_id": serde_json::Value::Null,
            "opencode_session_id": s.opencode_session_id,
        }));
    }

    let oc_sid = s.opencode_session_id.clone();
    s.mark_dead(Some(0));

    ok_json(serde_json::json!({
        "ok": true,
        "codex_session_id": serde_json::Value::Null,
        "opencode_session_id": oc_sid,
    }))
}

async fn handle_killall(
    daemon: Arc<RwLock<Daemon>>,
) -> serde_json::Value {
    // Collect all active session IDs
    let session_ids: Vec<String> = {
        let d = daemon.read().await;
        let mut ids = Vec::new();
        for (id, session) in &d.sessions {
            let s = session.lock().await;
            if s.state != SessionState::Dead {
                ids.push(id.clone());
            }
        }
        ids
    };

    let mut killed = Vec::new();
    for id in &session_ids {
        let session = {
            let d = daemon.read().await;
            match d.resolve_session(id) {
                Ok(s) => s,
                Err(_) => continue,
            }
        };

        let is_oc = {
            let s = session.lock().await;
            s.is_opencode()
        };
        let result = if is_oc {
            kill_opencode_session(&session).await
        } else {
            kill_session(&session).await
        };
        schedule_dead_session_cleanup(daemon.clone(), id.clone());

        let codex_session_id = result
            .get("codex_session_id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        killed.push(serde_json::json!({
            "session": id,
            "codex_session_id": codex_session_id,
        }));
    }

    ok_json(serde_json::json!({"ok": true, "killed": killed}))
}

async fn handle_gui_attach(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    let session = {
        let d = daemon.read().await;
        d.resolve_session(session_id)?
    };

    // Send replay of current screen
    let (replay_data, mut rx) = {
        let s = session.lock().await;
        let screen = s.parser.screen();
        let contents = screen.contents_formatted();
        let rx = s.pty_broadcast.subscribe();
        (contents, rx)
    };

    use base64::Engine;
    let replay_b64 = base64::engine::general_purpose::STANDARD.encode(&replay_data);
    let replay_msg = serde_json::json!({"type": "replay", "data": replay_b64});
    let json = serde_json::to_string(&replay_msg)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;

    // Stream new PTY bytes
    loop {
        match rx.recv().await {
            Ok(data) => {
                let data_b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                let msg = serde_json::json!({"type": "data", "data": data_b64});
                let json = serde_json::to_string(&msg)?;
                if writer.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                let eof_msg = serde_json::json!({"type": "eof"});
                let json = serde_json::to_string(&eof_msg)?;
                let _ = writer.write_all(json.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                break;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // Missed some messages, continue
                tracing::warn!("GUI attach lagged by {n} messages");
                continue;
            }
        }
    }

    Ok(())
}

async fn handle_log_follow(
    daemon: Arc<RwLock<Daemon>>,
    session_id: &str,
    since: Option<u64>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    let session = {
        let d = daemon.read().await;
        d.resolve_session(session_id)?
    };

    let (messages_path, mut cursor) = {
        let mut s = session.lock().await;
        let _ = s.log_writer.flush_all();
        let path = s.log_writer.messages_path().to_path_buf();
        let cursor = since.unwrap_or(s.read_cursor);
        (path, cursor)
    };

    // Send existing messages
    let existing = reader::read_since(&messages_path, cursor)?;
    for msg in &existing {
        let json = serde_json::to_string(msg)?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        cursor = msg.seq + 1;
    }

    // Stream new messages by polling
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    loop {
        interval.tick().await;

        // Force pending bytes to disk — opencode never ticks so its
        // BufWriter only flushes here. Codex flushes from its own tick too,
        // but a second flush here is harmless.
        {
            let mut s = session.lock().await;
            let _ = s.log_writer.flush_all();
        }

        let new_msgs = reader::read_since(&messages_path, cursor)?;
        for msg in &new_msgs {
            let json = serde_json::to_string(msg)?;
            if writer.write_all(json.as_bytes()).await.is_err() {
                return Ok(());
            }
            if writer.write_all(b"\n").await.is_err() {
                return Ok(());
            }
            cursor = msg.seq + 1;
        }

        // Check if session is dead
        let s = session.lock().await;
        if s.state == SessionState::Dead && new_msgs.is_empty() {
            break;
        }
    }

    Ok(())
}
