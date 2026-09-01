//! OpenCode backend: spawn `opencode run --format json`, parse NDJSON events,
//! translate into codex-ctl LogMessage format.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::log::LogMessage;

/// Spawn `opencode run --format json ...` as a short-lived subprocess.
///
/// Earlier revisions routed this through a persistent `opencode serve`
/// via `--attach` to dodge upstream hang #31109 on heavy resumed
/// sessions. As of opencode 1.18.x that hang is fixed AND `--attach`
/// itself drops model-response events on stdout — the workaround now
/// causes the failure it was meant to prevent. Direct `opencode run` in
/// 1.18 is verified to complete cleanly on both fresh and heavy
/// resumed sessions.
///
/// `binary` is the absolute path resolved by the client.
pub fn spawn_opencode_run(
    binary: &str,
    prompt: &str,
    cwd: &Path,
    session_id: Option<&str>,
    model: Option<&str>,
) -> Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.arg("run");
    cmd.arg("--format").arg("json");
    cmd.arg("--dir").arg(cwd);

    // Optional model override (`provider/model`). Absent => opencode's
    // configured default model (unchanged behavior).
    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }

    if let Some(sid) = session_id {
        cmd.arg("--continue").arg("--session").arg(sid);
    }

    cmd.arg(prompt);

    cmd.stdout(Stdio::piped());
    // Captured, not discarded: opencode reports fatal startup failures
    // ("Session not found", auth/config errors) only on stderr, and a
    // discarded stderr turned every one of them into a silent empty
    // session that looked like a successful no-op run.
    cmd.stderr(Stdio::piped());
    // Piped-stdin + write "\n" + close matches shell `echo | opencode
    // run`, which reliably makes opencode emit its NDJSON stream even
    // for tiny prompts. `Stdio::null()` alone sometimes stops the
    // stream after `step_start`.
    cmd.stdin(Stdio::piped());

    let mut child = cmd.spawn()
        .context("Failed to spawn opencode")?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        tokio::spawn(async move {
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.shutdown().await;
        });
    }

    Ok(child)
}

/// What a single `opencode run` produced. Carries enough detail for the
/// caller to tell a clean completion from a failure that produced no
/// output at all, which previously both looked like "idle, empty log".
pub struct RunOutcome {
    pub session_id: Option<String>,
    pub exit_code: Option<i32>,
    /// Message from an `{"type":"error"}` NDJSON event, if any.
    pub error: Option<String>,
    /// Whatever opencode wrote to stderr (fatal startup errors land here).
    pub stderr: String,
    /// True if the model produced any text or tool activity.
    pub saw_output: bool,
}

/// Translate opencode NDJSON events from a running child process,
/// writing log messages and blocks to the session log.
///
/// Locks the session mutex per-event to write to the log.
pub async fn consume_events(
    child: &mut Child,
    session: &tokio::sync::Mutex<super::Session>,
) -> Result<RunOutcome> {
    let stdout = child.stdout.take()
        .context("No stdout on opencode child")?;

    // Drain stderr concurrently so a chatty failure can't fill the pipe
    // buffer and deadlock the stdout reader.
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut rdr = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match rdr.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if buf.len() < 8192 {
                            buf.push_str(&line);
                        }
                    }
                }
            }
            buf
        })
    });

    let mut reader = BufReader::new(stdout);
    let mut line_buf = String::new();
    let mut opencode_session_id: Option<String> = None;
    let mut error: Option<String> = None;
    let mut saw_output = false;

    loop {
        line_buf.clear();
        let n = reader.read_line(&mut line_buf).await?;
        if n == 0 {
            break;
        }

        let trimmed = line_buf.trim();
        if trimmed.is_empty() {
            continue;
        }

        let event: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if opencode_session_id.is_none() {
            if let Some(sid) = event.get("sessionID").and_then(|v| v.as_str()) {
                opencode_session_id = Some(sid.to_string());
            }
        }

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let part = event.get("part").cloned().unwrap_or_default();

        // Errors arrive as their own top-level event with no `part`, so
        // they never reached `process_event`'s match and were dropped —
        // an unusable model or a server-side failure produced a session
        // that just went idle with an empty log. Surface them instead.
        if event_type == "error" {
            let err = event.get("error").cloned().unwrap_or_default();
            let name = err.get("name").and_then(|v| v.as_str()).unwrap_or("Error");
            let message = err
                .get("data")
                .and_then(|d| d.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = if message.is_empty() {
                name.to_string()
            } else {
                format!("{name}: {message}")
            };

            let mut s = session.lock().await;
            let msg = LogMessage::status(s.next_seq, format!("[opencode error] {text}"));
            s.next_seq += 1;
            let _ = s.log_writer.append_message(&msg);
            drop(s);

            error = Some(text);
            continue;
        }

        if event_type == "text" || event_type == "tool_use" {
            saw_output = true;
        }

        // Lock session, process event, unlock
        let mut s = session.lock().await;
        process_event(event_type, &part, &mut s);
        drop(s);
    }

    let status = child.wait().await?;
    let exit_code = status.code();

    let stderr = match stderr_task {
        Some(t) => t.await.unwrap_or_default(),
        None => String::new(),
    };

    Ok(RunOutcome {
        session_id: opencode_session_id,
        exit_code,
        error,
        stderr,
        saw_output,
    })
}

/// Process a single NDJSON event, writing to the session log.
fn process_event(
    event_type: &str,
    part: &serde_json::Value,
    s: &mut super::Session,
) {
    match event_type {
        "text" => {
            let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !text.trim().is_empty() {
                let msg = LogMessage::agent_output(s.next_seq, format!("\u{2022} {text}"));
                s.next_seq += 1;
                let _ = s.log_writer.append_message(&msg);
            }
        }

        "tool_use" => {
            let state = part.get("state").cloned().unwrap_or_default();
            let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status != "completed" && status != "error" {
                return;
            }

            let tool = part.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            let input = state.get("input").cloned().unwrap_or_default();
            let output_str = state.get("output").and_then(|v| v.as_str()).unwrap_or("");
            let title = state.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let metadata = state.get("metadata").cloned().unwrap_or_default();

            let bid = s.next_block_id;
            s.next_block_id += 1;

            match tool {
                "write" => {
                    let filepath = input.get("filePath").and_then(|v| v.as_str()).unwrap_or("");
                    let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let exists = metadata.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
                    let line_count = content.lines().count();
                    let verb = if exists { "Edited" } else { "Created" };
                    let basename = Path::new(filepath)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| filepath.to_string());

                    let header = format!("{verb} {basename} (+{line_count} -0)");
                    let body: Vec<String> = content.lines()
                        .enumerate()
                        .map(|(i, l)| format!("    {} +{l}", i + 1))
                        .collect();

                    let block_type = if exists { "edited" } else { "created" };
                    emit_block(s, bid, block_type, &header, body);
                }

                "edit" => {
                    let filepath = if !title.is_empty() { title } else { "unknown" };
                    let header = format!("Edited {filepath}");
                    let old_str = input.get("oldString").and_then(|v| v.as_str()).unwrap_or("");
                    let new_str = input.get("newString").and_then(|v| v.as_str()).unwrap_or("");
                    let mut body = Vec::new();
                    for l in old_str.lines() {
                        body.push(format!("- {l}"));
                    }
                    for l in new_str.lines() {
                        body.push(format!("+ {l}"));
                    }
                    if body.is_empty() && !output_str.is_empty() {
                        body.push(output_str.to_string());
                    }
                    emit_block(s, bid, "edited", &header, body);
                }

                "bash" => {
                    let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let description = input.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let exit_code = metadata.get("exit").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let truncated = metadata.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false);

                    let header = if !description.is_empty() {
                        format!("Ran {description}")
                    } else {
                        format!("Ran {command}")
                    };
                    let mut body: Vec<String> = output_str.lines().map(|l| l.to_string()).collect();
                    if truncated {
                        body.push("[output truncated]".to_string());
                    }

                    emit_block(s, bid, "ran", &header, body);

                    if exit_code != 0 {
                        let err_msg = LogMessage::status(s.next_seq, format!("[exit code: {exit_code}]"));
                        s.next_seq += 1;
                        let _ = s.log_writer.append_message(&err_msg);
                    }
                }

                "read" => {
                    let filepath = if !title.is_empty() { title } else { "unknown" };
                    let header = format!("Read {filepath}");
                    let body: Vec<String> = output_str.lines().map(|l| l.to_string()).collect();
                    emit_block(s, bid, "read", &header, body);
                }

                "todowrite" => {
                    let msg = LogMessage::status(s.next_seq, format!("[plan: {title}]"));
                    s.next_seq += 1;
                    let _ = s.log_writer.append_message(&msg);
                }

                _ => {
                    let header = format!("{tool}: {title}");
                    let body: Vec<String> = if !output_str.is_empty() {
                        output_str.lines().map(|l| l.to_string()).collect()
                    } else {
                        Vec::new()
                    };
                    emit_block(s, bid, tool, &header, body);
                }
            }

            if status == "error" {
                let err = LogMessage::status(
                    s.next_seq,
                    format!("[tool error: {tool}] {output_str}"),
                );
                s.next_seq += 1;
                let _ = s.log_writer.append_message(&err);
            }
        }

        "step_finish" => {
            let cost = part.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let reason = part.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let tokens = part.get("tokens")
                .and_then(|v| v.get("total"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if cost > 0.0 {
                let msg = LogMessage::status(
                    s.next_seq,
                    format!("[step: reason={reason} cost=${cost:.4} tokens={tokens}]"),
                );
                s.next_seq += 1;
                let _ = s.log_writer.append_message(&msg);
            }
        }

        _ => {}
    }
}

/// Streams body to `bodies.bin` and records metadata in memory + the message
/// log. Body bytes are not retained.
fn emit_block(
    s: &mut super::Session,
    bid: u64,
    block_type: &str,
    header: &str,
    body: Vec<String>,
) {
    let seq = s.next_seq;
    s.next_seq += 1;
    super::emit_block_to_disk(s, bid, seq, block_type, header, &body);
}
