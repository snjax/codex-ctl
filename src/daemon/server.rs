use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use tracing::{error, info};

use super::Daemon;
use super::handler;
use crate::protocol::Request;

/// Run the Unix socket server.
pub async fn run_server(daemon: Arc<RwLock<Daemon>>, sock_path: &Path) -> Result<()> {
    let listener = UnixListener::bind(sock_path)?;
    info!("Listening on {}", sock_path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(daemon, stream).await {
                error!("Connection error: {e}");
            }
        });
    }
}

async fn handle_connection(
    daemon: Arc<RwLock<Daemon>>,
    stream: tokio::net::UnixStream,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    // Read one request line
    let n = buf_reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(req) => req,
        Err(e) => {
            let resp = crate::protocol::err_json(&format!("Invalid request: {e}"));
            let json = serde_json::to_string(&resp)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            return Ok(());
        }
    };

    // Check if this is a streaming request (GuiAttach or Log --follow)
    let is_streaming = matches!(
        &request,
        Request::GuiAttach { .. }
            | Request::Log {
                follow: true,
                ..
            }
    );

    if is_streaming {
        // For streaming, we keep the writer open and let the handler write multiple lines
        handler::handle_streaming(daemon, request, writer).await?;
    } else {
        // Single response
        let response = handler::handle_request(daemon, request).await;
        let json = serde_json::to_string(&response)?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }

    Ok(())
}
