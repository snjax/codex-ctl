pub mod handler;
pub mod server;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::info;

use crate::client;
use crate::session::Session;

/// The daemon managing all sessions.
pub struct Daemon {
    pub sessions: HashMap<String, Arc<tokio::sync::Mutex<Session>>>,
    pub base_dir: PathBuf,
    pub sessions_dir: PathBuf,
}

impl Daemon {
    pub fn new() -> Result<Self> {
        let base_dir = client::base_dir();
        let sessions_dir = base_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;

        Ok(Daemon {
            sessions: HashMap::new(),
            base_dir,
            sessions_dir,
        })
    }

    /// Run the daemon: set up logging, write PID, start server.
    pub async fn run(self) -> Result<()> {
        let base_dir = self.base_dir.clone();

        // Set up file logging
        let log_path = base_dir.join("daemon.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_target(false)
            .init();

        // Write PID file
        let pid_path = client::pid_path();
        std::fs::write(&pid_path, std::process::id().to_string())?;
        info!("Daemon started, PID {}", std::process::id());

        // Remove stale socket
        let sock_path = client::socket_path();
        if sock_path.exists() {
            let _ = std::fs::remove_file(&sock_path);
        }

        // Start server
        let daemon = Arc::new(RwLock::new(self));

        // Handle SIGTERM/SIGINT for graceful shutdown
        let daemon_clone = daemon.clone();
        let sock_path_clone = sock_path.clone();
        let pid_path_clone = pid_path.clone();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Failed to register SIGTERM handler");
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("Failed to register SIGINT handler");

            tokio::select! {
                _ = sigterm.recv() => info!("Received SIGTERM"),
                _ = sigint.recv() => info!("Received SIGINT"),
            }

            info!("Shutting down daemon...");

            // Kill all sessions
            let daemon = daemon_clone.read().await;
            for (id, session) in &daemon.sessions {
                let mut session = session.lock().await;
                if session.state != crate::session::state::SessionState::Dead {
                    info!("Killing session {id}");
                    let _ = nix::sys::signal::kill(session.pid, nix::sys::signal::Signal::SIGTERM);
                    session.mark_dead(None);
                }
            }

            // Clean up files
            let _ = std::fs::remove_file(&sock_path_clone);
            let _ = std::fs::remove_file(&pid_path_clone);

            info!("Daemon shutdown complete");
            std::process::exit(0);
        });

        server::run_server(daemon, &sock_path).await
    }

    /// Resolve a session ID prefix to a full session.
    /// Returns error if ambiguous or not found.
    pub fn resolve_session(&self, prefix: &str) -> Result<Arc<tokio::sync::Mutex<Session>>> {
        let matches: Vec<_> = self
            .sessions
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .collect();

        match matches.len() {
            0 => anyhow::bail!("No session matching '{prefix}'"),
            1 => Ok(matches[0].1.clone()),
            _ => {
                let ids: Vec<_> = matches.iter().map(|(id, _)| id.as_str()).collect();
                anyhow::bail!(
                    "Ambiguous session prefix '{prefix}': matches {}",
                    ids.join(", ")
                );
            }
        }
    }
}
