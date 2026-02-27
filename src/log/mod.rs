pub mod formatter;
pub mod reader;

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::parser::prompt::PromptInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMessage {
    pub seq: u64,
    pub ts: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptInfo>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_lines: Option<u32>,
}

impl LogMessage {
    pub fn agent_output(seq: u64, text: String) -> Self {
        LogMessage {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type: "agent_output".into(),
            text,
            state_from: None,
            state_to: None,
            prompt: None,
            block_id: None,
            block_type: None,
            block_lines: None,
        }
    }

    pub fn status(seq: u64, text: String) -> Self {
        LogMessage {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type: "status".into(),
            text,
            state_from: None,
            state_to: None,
            prompt: None,
            block_id: None,
            block_type: None,
            block_lines: None,
        }
    }

    pub fn state_change(seq: u64, from: &str, to: &str) -> Self {
        LogMessage {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type: "state_change".into(),
            text: format!("{from} → {to}"),
            state_from: Some(from.into()),
            state_to: Some(to.into()),
            prompt: None,
            block_id: None,
            block_type: None,
            block_lines: None,
        }
    }

    pub fn block(seq: u64, header: &str, block_id: u64, block_type: &str, body_lines: u32) -> Self {
        LogMessage {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type: "block".into(),
            text: header.into(),
            state_from: None,
            state_to: None,
            prompt: None,
            block_id: Some(block_id),
            block_type: Some(block_type.into()),
            block_lines: Some(body_lines),
        }
    }

    pub fn prompt_msg(seq: u64, info: PromptInfo) -> Self {
        LogMessage {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type: "prompt".into(),
            text: info.question_text.clone(),
            state_from: None,
            state_to: None,
            prompt: Some(info),
            block_id: None,
            block_type: None,
            block_lines: None,
        }
    }
}

/// Append-only JSONL writer for log messages.
pub struct LogWriter {
    messages_writer: BufWriter<File>,
    blocks_writer: BufWriter<File>,
    messages_path: PathBuf,
    #[allow(dead_code)]
    blocks_path: PathBuf,
}

impl LogWriter {
    pub fn new(session_dir: &Path) -> Result<Self> {
        let messages_path = session_dir.join("messages.jsonl");
        let blocks_path = session_dir.join("blocks.jsonl");

        let messages_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&messages_path)?;
        let blocks_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&blocks_path)?;

        Ok(LogWriter {
            messages_writer: BufWriter::new(messages_file),
            blocks_writer: BufWriter::new(blocks_file),
            messages_path,
            blocks_path,
        })
    }

    pub fn append_message(&mut self, msg: &LogMessage) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        writeln!(self.messages_writer, "{json}")?;
        self.messages_writer.flush()?;
        Ok(())
    }

    pub fn append_block(&mut self, block: &crate::parser::blocks::Block) -> Result<()> {
        let json = serde_json::to_string(block)?;
        writeln!(self.blocks_writer, "{json}")?;
        self.blocks_writer.flush()?;
        Ok(())
    }

    pub fn messages_path(&self) -> &Path {
        &self.messages_path
    }

    #[allow(dead_code)]
    pub fn blocks_path(&self) -> &Path {
        &self.blocks_path
    }
}
