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
    /// Byte offset into the session's bodies.bin where this block's body lives.
    /// Allows recovering full body text after a daemon restart without keeping
    /// it in RAM.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub body_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub body_len: Option<u64>,
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
            body_offset: None,
            body_len: None,
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
            body_offset: None,
            body_len: None,
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
            body_offset: None,
            body_len: None,
        }
    }

    pub fn block(
        seq: u64,
        header: &str,
        block_id: u64,
        block_type: &str,
        body_lines: u32,
        body_offset: u64,
        body_len: u64,
    ) -> Self {
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
            body_offset: Some(body_offset),
            body_len: Some(body_len),
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
            body_offset: None,
            body_len: None,
        }
    }
}

/// Append-only writer for session log streams.
///
/// `messages.jsonl` holds structured events (small JSON per line). `bodies.bin`
/// is a raw, append-only byte file containing the bodies of all blocks (file
/// contents, command outputs, etc.) concatenated together. Block log entries
/// in messages.jsonl carry `body_offset`/`body_len` so a reader can extract
/// the exact bytes via `pread`. Keeping bodies on disk is what stops
/// `Session.blocks` from holding multi-GB of file contents in RAM.
///
/// Both files are buffered. Per-write `flush()` is intentionally avoided —
/// it would burn one syscall per event under heavy agent workloads. The
/// daemon calls [`LogWriter::flush_all`] from `Session::tick` (~20 Hz) so
/// live readers see fresh data within a frame, while batching syscalls.
pub struct LogWriter {
    messages_writer: BufWriter<File>,
    bodies_writer: BufWriter<File>,
    messages_path: PathBuf,
    bodies_path: PathBuf,
    /// Running byte offset into `bodies.bin`. Returned by [`append_body`] as
    /// the offset of the just-written body. Tracked in-process; not read back
    /// from disk because there is only one writer (the owning session, behind
    /// its Mutex).
    bodies_len: u64,
}

impl LogWriter {
    pub fn new(session_dir: &Path) -> Result<Self> {
        let messages_path = session_dir.join("messages.jsonl");
        let bodies_path = session_dir.join("bodies.bin");

        let messages_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&messages_path)?;
        let bodies_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&bodies_path)?;

        // Resume the in-memory offset from existing file size — keeps reads
        // correct if a session dir is reused (e.g. test runners).
        let bodies_len = bodies_file.metadata()?.len();

        Ok(LogWriter {
            messages_writer: BufWriter::new(messages_file),
            bodies_writer: BufWriter::new(bodies_file),
            messages_path,
            bodies_path,
            bodies_len,
        })
    }

    pub fn append_message(&mut self, msg: &LogMessage) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        writeln!(self.messages_writer, "{json}")?;
        Ok(())
    }

    /// Append a block body to `bodies.bin`. Returns the byte offset where the
    /// body was written; caller persists that in the corresponding
    /// `LogMessage::block` entry.
    ///
    /// Large bodies (> BufWriter capacity) bypass the buffer and hit the
    /// underlying file directly with a single `write_all` — exactly what we
    /// want for multi-MB tool outputs.
    pub fn append_body(&mut self, bytes: &[u8]) -> Result<u64> {
        let offset = self.bodies_len;
        self.bodies_writer.write_all(bytes)?;
        self.bodies_len += bytes.len() as u64;
        Ok(offset)
    }

    /// Flush both writers. Cheap (one or two `write()` syscalls each); meant
    /// to be called from `Session::tick` so live readers see recent events.
    pub fn flush_all(&mut self) -> Result<()> {
        self.messages_writer.flush()?;
        self.bodies_writer.flush()?;
        Ok(())
    }

    pub fn messages_path(&self) -> &Path {
        &self.messages_path
    }

    pub fn bodies_path(&self) -> &Path {
        &self.bodies_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn append_body_offsets_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = LogWriter::new(dir.path()).unwrap();

        let off1 = w.append_body(b"hello\nworld").unwrap();
        let off2 = w.append_body(b"second body").unwrap();
        let off3 = w.append_body(b"").unwrap();
        let off4 = w.append_body(b"x").unwrap();

        // Offsets are running byte counters, including the empty append.
        assert_eq!(off1, 0);
        assert_eq!(off2, 11);
        assert_eq!(off3, 22);
        assert_eq!(off4, 22);

        // After flush, the file holds exactly the concatenation we wrote.
        w.flush_all().unwrap();
        let mut buf = Vec::new();
        std::fs::File::open(w.bodies_path())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert_eq!(buf, b"hello\nworldsecond bodyx");
    }

    #[test]
    fn append_message_does_not_flush_per_write() {
        // Sanity check that `append_message` no longer flushes on every
        // call — the new policy is "BufWriter batches; tick() flushes".
        let dir = tempfile::tempdir().unwrap();
        let mut w = LogWriter::new(dir.path()).unwrap();
        let msg = LogMessage::status(1, "hi".into());
        w.append_message(&msg).unwrap();

        // Nothing is on disk yet because no one flushed.
        let on_disk = std::fs::read(w.messages_path()).unwrap();
        assert!(on_disk.is_empty(), "expected pending in BufWriter, got {} bytes", on_disk.len());

        w.flush_all().unwrap();
        let on_disk = std::fs::read(w.messages_path()).unwrap();
        assert!(!on_disk.is_empty());
    }

    #[test]
    fn new_writer_resumes_bodies_len_from_disk() {
        // If a session dir is reused (test runner, daemon restart with the
        // same dir), the running offset must continue from the existing
        // file size so subsequent reads stay correct.
        let dir = tempfile::tempdir().unwrap();
        let bodies_path = dir.path().join("bodies.bin");
        std::fs::write(&bodies_path, b"pre-existing bytes").unwrap();
        let mut w = LogWriter::new(dir.path()).unwrap();
        let off = w.append_body(b"new").unwrap();
        assert_eq!(off, 18);
    }

    #[test]
    fn pread_after_append_matches_expand_path() {
        // Mirrors what `handle_expand` does: write several bodies, then
        // open the file fresh and seek+read the recorded ranges. This is
        // the load-bearing invariant of the whole bodies.bin design.
        use std::io::{Read, Seek, SeekFrom};

        let dir = tempfile::tempdir().unwrap();
        let mut w = LogWriter::new(dir.path()).unwrap();

        let bodies: Vec<&[u8]> = vec![
            b"hello world",
            b"second\nblock\nwith\nlines",
            b"",
            b"final",
        ];
        let mut recorded: Vec<(u64, u64)> = Vec::new();
        for b in &bodies {
            let off = w.append_body(b).unwrap();
            recorded.push((off, b.len() as u64));
        }
        w.flush_all().unwrap();

        let mut file = std::fs::File::open(w.bodies_path()).unwrap();
        for (i, &(off, len)) in recorded.iter().enumerate() {
            let mut buf = vec![0u8; len as usize];
            if len > 0 {
                file.seek(SeekFrom::Start(off)).unwrap();
                file.read_exact(&mut buf).unwrap();
            }
            assert_eq!(buf, bodies[i], "body {i} round-trips through pread");
        }
    }
}
