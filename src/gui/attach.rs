use std::io::Write;

use anyhow::Result;

use crate::client;

/// Run the _gui-attach command: connect to daemon, receive replay + stream.
pub async fn run_gui_attach(session_id: &str) -> Result<()> {
    let mut stream = client::connect().await?;

    let req = crate::protocol::Request::GuiAttach {
        session: session_id.into(),
    };
    client::send_request(&mut stream, &req).await?;

    // Read responses as streaming NDJSON
    let mut stdout = std::io::stdout();
    let mut buf = String::new();

    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(&mut stream);

    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            break; // EOF
        }

        let line = buf.trim();
        if line.is_empty() {
            continue;
        }

        let msg: serde_json::Value = serde_json::from_str(line)?;

        match msg.get("type").and_then(|t| t.as_str()) {
            Some("replay") | Some("data") => {
                if let Some(data_b64) = msg.get("data").and_then(|d| d.as_str()) {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                }
            }
            Some("eof") => {
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
