use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::protocol::Request;

/// Get the base directory for codex-ctl state.
pub fn base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEX_CTL_DIR") {
        PathBuf::from(dir)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".codex-ctl")
    }
}

/// Get the socket path.
pub fn socket_path() -> PathBuf {
    base_dir().join("daemon.sock")
}

/// Get the PID file path.
pub fn pid_path() -> PathBuf {
    base_dir().join("daemon.pid")
}

/// Connect to the daemon socket.
pub async fn connect() -> Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path)
        .await
        .with_context(|| format!("Cannot connect to daemon at {}", path.display()))
}

/// Send a request and receive a single JSON response line.
pub async fn send_request(
    stream: &mut UnixStream,
    request: &Request,
) -> Result<serde_json::Value> {
    let json = serde_json::to_string(request)?;
    stream.write_all(json.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: serde_json::Value =
        serde_json::from_str(line.trim()).context("Invalid JSON response from daemon")?;
    Ok(response)
}

/// Send a request and receive a response, consuming the stream for single-shot use.
pub async fn request(req: &Request) -> Result<serde_json::Value> {
    let mut stream = connect().await?;
    send_request(&mut stream, req).await
}
