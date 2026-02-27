use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;

use super::LogMessage;

/// Read all log messages from a JSONL file.
pub fn read_all(path: &Path) -> Result<Vec<LogMessage>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: LogMessage = serde_json::from_str(&line)?;
        messages.push(msg);
    }
    Ok(messages)
}

/// Read log messages with seq >= since.
pub fn read_since(path: &Path, since: u64) -> Result<Vec<LogMessage>> {
    let all = read_all(path)?;
    Ok(all.into_iter().filter(|m| m.seq >= since).collect())
}

/// Read unread messages (seq > cursor) and return the new cursor.
#[allow(dead_code)]
pub fn read_unread(path: &Path, cursor: u64) -> Result<(Vec<LogMessage>, u64)> {
    let messages = read_since(path, cursor)?;
    let new_cursor = messages.iter().map(|m| m.seq + 1).max().unwrap_or(cursor);
    Ok((messages, new_cursor))
}

/// Read blocks from blocks.jsonl.
#[allow(dead_code)]
pub fn read_blocks(path: &Path) -> Result<Vec<crate::parser::blocks::Block>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut blocks = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let block: crate::parser::blocks::Block = serde_json::from_str(&line)?;
        blocks.push(block);
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_read_all_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        File::create(&path).unwrap();
        let msgs = read_all(&path).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_read_all_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        let mut f = File::create(&path).unwrap();
        let msg = LogMessage::agent_output(1, "hello".into());
        writeln!(f, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        let msg = LogMessage::agent_output(2, "world".into());
        writeln!(f, "{}", serde_json::to_string(&msg).unwrap()).unwrap();

        let msgs = read_all(&path).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].seq, 1);
        assert_eq!(msgs[1].seq, 2);
    }

    #[test]
    fn test_read_since() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        let mut f = File::create(&path).unwrap();
        for i in 1..=5 {
            let msg = LogMessage::agent_output(i, format!("msg {i}"));
            writeln!(f, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        }

        let msgs = read_since(&path, 3).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].seq, 3);
    }

    #[test]
    fn test_read_unread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        let mut f = File::create(&path).unwrap();
        for i in 1..=5 {
            let msg = LogMessage::agent_output(i, format!("msg {i}"));
            writeln!(f, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        }

        let (msgs, cursor) = read_unread(&path, 3).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(cursor, 6);
    }

    #[test]
    fn test_read_nonexistent() {
        let path = Path::new("/tmp/nonexistent_test_codex_ctl.jsonl");
        let msgs = read_all(path).unwrap();
        assert!(msgs.is_empty());
    }
}
