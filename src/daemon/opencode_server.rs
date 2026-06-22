//! Persistent `opencode serve` process owned by the daemon.
//!
//! Short-lived `opencode run --continue --session ...` hangs without exiting
//! on heavy/long upstream sessions even after the agent loop completes
//! (upstream issue sst/opencode#31109). Running one `opencode serve` per
//! daemon and using `opencode run --attach <url>` for every spawn avoids the
//! buggy lifecycle entirely.
//!
//! The server is loopback-only (`127.0.0.1`), uses a kernel-picked port via
//! `--port 0`, is owned as a direct child of the daemon, and is killed in the
//! daemon shutdown hook. A pidfile at `<base>/opencode-server.pid` lets a
//! restarted daemon clean up an orphaned server from a previous run.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{info, warn};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const LISTENING_MARKER: &str = "http://127.0.0.1:";

pub struct OpencodeServer {
    pub url: String,
    child: Child,
    pid_path: PathBuf,
}

impl OpencodeServer {
    /// Spawn `opencode serve --port 0 --hostname 127.0.0.1` and discover the
    /// chosen port from the "listening on http://127.0.0.1:<port>" line.
    pub async fn start(binary: &str, pid_path: PathBuf) -> Result<Self> {
        let mut cmd = Command::new(binary);
        cmd.arg("serve")
            .arg("--port").arg("0")
            .arg("--hostname").arg("127.0.0.1");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Detach from any terminal-driven signals; the daemon controls
            // lifetime via the kept Child handle.
            .kill_on_drop(false);

        let mut child = cmd.spawn().context("Failed to spawn `opencode serve`")?;

        let stdout = child.stdout.take().context("no stdout on opencode serve")?;
        let stderr = child.stderr.take().context("no stderr on opencode serve")?;

        let parse = parse_listening_url(stdout);
        let (url, stdout_back) = match tokio::time::timeout(STARTUP_TIMEOUT, parse).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                let _ = child.kill().await;
                return Err(e);
            }
            Err(_) => {
                let _ = child.kill().await;
                bail!("Timeout waiting for opencode serve to report its listening URL");
            }
        };

        // Drain stdout and stderr forever so the server doesn't block on a
        // full pipe. We're not interested in their content past the URL line;
        // serve writes structured logs to its own logfile already.
        spawn_drain(stdout_back, "opencode-serve stdout");
        spawn_drain(stderr, "opencode-serve stderr");

        // Best-effort pidfile: lets a restarted daemon kill an orphan from
        // a previous crash. We accept the small TOCTOU window on PID recycle.
        if let Some(pid) = child.id() {
            if let Err(e) = std::fs::write(&pid_path, pid.to_string()) {
                warn!("Failed to write opencode-server pidfile {}: {e}", pid_path.display());
            }
        }

        info!("opencode server up at {url}");
        Ok(OpencodeServer { url, child, pid_path })
    }

    /// Has the child exited unexpectedly? Used by the lazy-init path to
    /// detect a dead-but-not-collected server and respawn.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// SIGTERM the server, wait briefly, escalate to SIGKILL via `kill()`.
    /// Removes the pidfile on success.
    pub async fn shutdown(mut self) {
        info!("Shutting down opencode server at {}", self.url);
        if let Some(pid) = self.child.id() {
            let pid = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }
        // Bounded wait — `child.kill()` sends SIGKILL if the wait races out.
        let killed = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        if killed.is_err() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

/// On daemon startup, reap an orphaned server from a previous run. Safe even
/// if the pidfile is stale (process already gone) or contains garbage.
pub fn cleanup_stale(pid_path: &std::path::Path) {
    let Ok(pid_str) = std::fs::read_to_string(pid_path) else { return };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(pid_path);
        return;
    };
    // Guard PID-recycling: only kill if /proc/<pid>/cmdline names opencode.
    let cmdline_path = format!("/proc/{pid}/cmdline");
    if let Ok(cmdline) = std::fs::read(&cmdline_path) {
        if cmdline.windows(b"opencode".len()).any(|w| w == b"opencode") {
            let p = nix::unistd::Pid::from_raw(pid);
            info!("Reaping orphaned opencode server pid={pid}");
            let _ = nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGTERM);
        }
    }
    let _ = std::fs::remove_file(pid_path);
}

async fn parse_listening_url(
    stdout: tokio::process::ChildStdout,
) -> Result<(String, tokio::process::ChildStdout)> {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            bail!("opencode serve closed stdout before reporting its URL");
        }
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find(LISTENING_MARKER) {
            let rest = &trimmed[idx..];
            // The URL ends at first whitespace; the line is e.g.
            // "opencode server listening on http://127.0.0.1:54321".
            let end = rest
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            return Ok((rest[..end].to_string(), reader.into_inner()));
        }
    }
}

fn spawn_drain<R>(stream: R, label: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut r = BufReader::new(stream);
        let mut buf = String::new();
        loop {
            buf.clear();
            match r.read_line(&mut buf).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    warn!("{label} drain error: {e}");
                    break;
                }
            }
        }
    });
}
