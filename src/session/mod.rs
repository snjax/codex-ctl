pub mod input;
pub mod pty;
pub mod screen;
pub mod stabilizer;
pub mod state;
pub mod wait;

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::log::{LogMessage, LogWriter};
use crate::parser::blocks::{self, Block};
use crate::parser::classifier;
use crate::session::screen::{compute_diff, filter_lines, take_snapshot};
use crate::session::stabilizer::Stabilizer;
use crate::session::state::{DetectedState, SessionState, detect_state};

/// Metadata written to disk for each session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub pid: i32,
    pub cwd: String,
    pub created_at: String,
    pub prompt: String,
}

/// A running Codex session.
pub struct Session {
    pub id: String,
    pub pid: nix::unistd::Pid,
    pub master_fd: RawFd,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub prompt: String,
    pub session_dir: PathBuf,

    // VT terminal parser
    pub parser: vt100::Parser,

    // State tracking
    pub state: SessionState,
    pub state_tx: watch::Sender<SessionState>,
    pub state_rx: watch::Receiver<SessionState>,
    pub last_esc_seen: Instant,

    // Stabilizer
    pub stabilizer: Stabilizer,
    pub stable_snapshot: Vec<String>,

    // Blocks
    pub next_block_id: u64,
    pub blocks: HashMap<u64, Block>,

    // PTY broadcast for GUI
    pub pty_broadcast: broadcast::Sender<Bytes>,

    // Log
    pub log_writer: LogWriter,
    pub next_seq: u64,
    pub read_cursor: u64,

    // GUI
    pub gui_pid: Option<u32>,

    // Exit code
    pub exit_code: Option<i32>,

    // Whether "esc to interrupt" has ever been seen (prevents spurious idle during startup)
    pub has_seen_esc: bool,

    // Codex's own session UUID (captured from terminal output during graceful kill)
    pub codex_session_id: Option<String>,
}

impl Session {
    /// Create a new session from a PTY spawn result.
    #[allow(dead_code)]
    pub fn new(
        spawn_result: pty::SpawnResult,
        prompt: &str,
        cwd: &Path,
        session_dir: &Path,
    ) -> Result<Self> {
        let id = Uuid::new_v4().to_string()[..8].to_string();
        let (state_tx, state_rx) = watch::channel(SessionState::Working);
        let (pty_broadcast, _) = broadcast::channel(256);
        let log_writer = LogWriter::new(session_dir)?;

        Ok(Session {
            id,
            pid: spawn_result.pid,
            master_fd: spawn_result.master_fd.as_raw_fd(),
            cwd: cwd.to_path_buf(),
            created_at: Utc::now(),
            prompt: prompt.to_string(),
            session_dir: session_dir.to_path_buf(),
            parser: vt100::Parser::new(500, 200, 0),
            state: SessionState::Working,
            state_tx,
            state_rx,
            last_esc_seen: Instant::now(),
            stabilizer: Stabilizer::default_delay(),
            stable_snapshot: Vec::new(),
            next_block_id: 1,
            blocks: HashMap::new(),
            pty_broadcast,
            log_writer,
            next_seq: 1,
            read_cursor: 0,
            gui_pid: None,
            exit_code: None,
            has_seen_esc: false,
            codex_session_id: None,
        })
    }

    /// Get the owned fd (we need to leak it since Session stores RawFd for the read loop).
    /// This is called once during session creation to keep the OwnedFd alive.
    pub fn new_with_owned_fd(
        spawn_result: pty::SpawnResult,
        prompt: &str,
        cwd: &Path,
        session_dir: &Path,
    ) -> Result<(Self, OwnedFd)> {
        let id = Uuid::new_v4().to_string()[..8].to_string();
        let (state_tx, state_rx) = watch::channel(SessionState::Working);
        let (pty_broadcast, _) = broadcast::channel(256);
        let log_writer = LogWriter::new(session_dir)?;

        let raw_fd = spawn_result.master_fd.as_raw_fd();
        let session = Session {
            id,
            pid: spawn_result.pid,
            master_fd: raw_fd,
            cwd: cwd.to_path_buf(),
            created_at: Utc::now(),
            prompt: prompt.to_string(),
            session_dir: session_dir.to_path_buf(),
            parser: vt100::Parser::new(500, 200, 0),
            state: SessionState::Working,
            state_tx,
            state_rx,
            last_esc_seen: Instant::now(),
            stabilizer: Stabilizer::default_delay(),
            stable_snapshot: Vec::new(),
            next_block_id: 1,
            blocks: HashMap::new(),
            pty_broadcast,
            log_writer,
            next_seq: 1,
            read_cursor: 0,
            gui_pid: None,
            exit_code: None,
            has_seen_esc: false,
            codex_session_id: None,
        };
        Ok((session, spawn_result.master_fd))
    }

    /// Write session metadata to disk.
    pub fn write_meta(&self) -> Result<()> {
        let meta = SessionMeta {
            id: self.id.clone(),
            pid: self.pid.as_raw(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            created_at: self.created_at.to_rfc3339(),
            prompt: self.prompt.clone(),
        };
        let json = serde_json::to_string_pretty(&meta)?;
        std::fs::write(self.session_dir.join("meta.json"), json)?;
        Ok(())
    }

    /// Process incoming PTY bytes.
    pub fn on_pty_data(&mut self, data: &[u8]) {
        self.parser.process(data);
        let snapshot = take_snapshot(&self.parser);
        let filtered = filter_lines(&snapshot);

        // Instant state detection (no debounce)
        let now = Instant::now();
        if filtered.iter().any(|l| l.contains("esc to interrupt")) {
            self.last_esc_seen = now;
            self.has_seen_esc = true;
        }
        let mut detected = detect_state(&filtered, self.last_esc_seen, now);
        // Don't allow idle until we've seen "esc to interrupt" at least once
        if detected.state == SessionState::Idle && !self.has_seen_esc {
            detected.state = SessionState::Working;
        }
        self.update_state(detected);

        // Feed stabilizer for log emission (stripped of UI chrome)
        let content_only = screen::strip_ui_chrome(&filtered);
        self.stabilizer.on_change(content_only);
    }

    /// Try to commit stable content and emit log messages.
    /// Also re-evaluate state (handles Working→Idle when PTY goes silent).
    pub fn tick(&mut self) {
        if let Some(committed) = self.stabilizer.try_commit() {
            self.commit(committed);
        }

        // Re-evaluate state on every tick so that Working→Idle
        // transition fires even when the PTY is silent.
        let now = Instant::now();
        let snapshot = take_snapshot(&self.parser);
        let filtered = filter_lines(&snapshot);
        if filtered.iter().any(|l| l.contains("esc to interrupt")) {
            self.last_esc_seen = now;
            self.has_seen_esc = true;
        }
        let mut detected = detect_state(&filtered, self.last_esc_seen, now);
        // Don't allow idle until we've seen "esc to interrupt" at least once
        if detected.state == SessionState::Idle && !self.has_seen_esc {
            detected.state = SessionState::Working;
        }
        self.update_state(detected);
    }

    fn commit(&mut self, new_snapshot: Vec<String>) {
        let diff = compute_diff(&self.stable_snapshot, &new_snapshot);
        if !diff.is_empty() {
            let detected_blocks = blocks::detect_blocks(&diff);
            let remaining = blocks::extract_non_block_text(&diff, &detected_blocks);

            // Emit blocks
            for mut block in detected_blocks {
                block.id = self.next_block_id;
                self.next_block_id += 1;
                block.seq = self.next_seq;

                let body_lines = block.body.len() as u32;
                let msg = LogMessage::block(
                    self.next_seq,
                    &block.header,
                    block.id,
                    &block.block_type,
                    body_lines,
                );
                self.next_seq += 1;
                let _ = self.log_writer.append_message(&msg);
                let _ = self.log_writer.append_block(&block);
                self.blocks.insert(block.id, block);
            }

            // Emit remaining text
            for line in &remaining {
                let msg_type = classifier::classify_line(line);
                let msg = match msg_type {
                    classifier::MsgType::Status => {
                        LogMessage::status(self.next_seq, line.clone())
                    }
                    classifier::MsgType::Prompt => {
                        // Prompt details handled by state detection
                        LogMessage::status(self.next_seq, line.clone())
                    }
                    _ => LogMessage::agent_output(self.next_seq, line.clone()),
                };
                self.next_seq += 1;
                let _ = self.log_writer.append_message(&msg);
            }
        }
        self.stable_snapshot = new_snapshot;
    }

    fn update_state(&mut self, detected: DetectedState) {
        let new_state = detected.state;
        if new_state != self.state {
            let old_str = self.state.to_string();
            let new_str = new_state.to_string();

            // Emit state change log
            let msg = LogMessage::state_change(self.next_seq, &old_str, &new_str);
            self.next_seq += 1;
            let _ = self.log_writer.append_message(&msg);

            // Emit prompt info if transitioning to prompting
            if let Some(info) = detected.prompt_info {
                if new_state == SessionState::Prompting
                    || new_state == SessionState::PromptingNotes
                {
                    let msg = LogMessage::prompt_msg(self.next_seq, info);
                    self.next_seq += 1;
                    let _ = self.log_writer.append_message(&msg);
                }
            }

            self.state = new_state.clone();
            let _ = self.state_tx.send(new_state);
        }
    }

    /// Mark session as dead.
    pub fn mark_dead(&mut self, exit_code: Option<i32>) {
        self.exit_code = exit_code;
        self.state = SessionState::Dead;
        let _ = self.state_tx.send(SessionState::Dead);

        let msg = LogMessage::state_change(
            self.next_seq,
            &self.state.to_string(),
            "dead",
        );
        self.next_seq += 1;
        let _ = self.log_writer.append_message(&msg);
    }

    /// Get a snapshot of the current screen.
    pub fn screen_lines(&self) -> Vec<String> {
        let snapshot = take_snapshot(&self.parser);
        snapshot
    }

    /// Get the current screen lines (filtered).
    pub fn screen_lines_filtered(&self) -> Vec<String> {
        filter_lines(&self.screen_lines())
    }

    /// Get session info as JSON value.
    pub fn info_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "state": self.state.to_string(),
            "cwd": self.cwd.to_string_lossy(),
            "created_at": self.created_at.to_rfc3339(),
            "prompt": self.prompt,
        })
    }
}
