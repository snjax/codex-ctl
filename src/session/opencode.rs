//! OpenCode backend: spawn `opencode run --format json`, parse NDJSON events,
//! translate into codex-ctl LogMessage format.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::log::LogMessage;
use crate::parser::blocks::Block;

/// Find the opencode binary.
fn find_opencode_binary() -> Result<String> {
    if let Ok(path) = std::env::var("CODEX_CTL_OPENCODE_PATH") {
        return Ok(path);
    }
    let path = which::which("opencode")
        .context("Cannot find 'opencode' in PATH. Set $CODEX_CTL_OPENCODE_PATH.")?;
    Ok(path.to_string_lossy().into_owned())
}

/// Spawn `opencode run --format json` as a subprocess.
pub fn spawn_opencode_run(
    prompt: &str,
    cwd: &Path,
    session_id: Option<&str>,
) -> Result<Child> {
    let binary = find_opencode_binary()?;
    let mut cmd = Command::new(&binary);
    cmd.arg("run");
    cmd.arg("--format").arg("json");
    cmd.arg("--dir").arg(cwd);

    if let Some(sid) = session_id {
        cmd.arg("--continue").arg("--session").arg(sid);
    }

    cmd.arg(prompt);

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    cmd.stdin(Stdio::null());

    let child = cmd.spawn()
        .context("Failed to spawn opencode")?;

    Ok(child)
}

/// Translate opencode NDJSON events from a running child process,
/// writing log messages and blocks to the session log.
///
/// Locks the session mutex per-event to write to the log.
///
/// Returns `(opencode_session_id, exit_code)`.
pub async fn consume_events(
    child: &mut Child,
    session: &tokio::sync::Mutex<super::Session>,
) -> Result<(Option<String>, Option<i32>)> {
    let stdout = child.stdout.take()
        .context("No stdout on opencode child")?;

    let mut reader = BufReader::new(stdout);
    let mut line_buf = String::new();
    let mut opencode_session_id: Option<String> = None;

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

        // Lock session, process event, unlock
        let mut s = session.lock().await;
        process_event(event_type, &part, &mut s);
        drop(s);
    }

    let status = child.wait().await?;
    let exit_code = status.code();

    Ok((opencode_session_id, exit_code))
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

/// Helper: emit a block + log message.
fn emit_block(
    s: &mut super::Session,
    bid: u64,
    block_type: &str,
    header: &str,
    body: Vec<String>,
) {
    let block = Block {
        id: bid,
        block_type: block_type.to_string(),
        header: header.to_string(),
        body: body.clone(),
        seq: s.next_seq,
    };
    let msg = LogMessage::block(s.next_seq, header, bid, block_type, body.len() as u32);
    s.next_seq += 1;
    let _ = s.log_writer.append_message(&msg);
    let _ = s.log_writer.append_block(&block);
    s.blocks.insert(bid, block);
}
