use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static BLOCK_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[•]\s+(Edited|Created|Deleted|Ran|Read)\s+(.+)$").unwrap()
});

/// A detected block with header and body lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: u64,
    pub block_type: String,
    pub header: String,
    pub body: Vec<String>,
    pub seq: u64,
}

#[derive(Debug)]
#[allow(dead_code)]
struct BlockHeader {
    block_type: String,
    text: String,
}

/// Parse a block header line, returning the block type and header text.
pub fn parse_block_header(line: &str) -> Option<(String, String)> {
    BLOCK_HEADER_RE.captures(line).map(|caps| {
        let block_type = caps[1].to_lowercase();
        // Handle "Ran" -> "ran_command"
        let block_type = if block_type == "ran" {
            "ran_command".to_string()
        } else {
            block_type
        };
        let text = line
            .trim_start_matches('•')
            .trim_start_matches('\u{2022}')
            .trim()
            .to_string();
        (block_type, text)
    })
}

/// Check if a line looks like block body content (indented or empty).
fn is_block_body_line(line: &str) -> bool {
    line.is_empty() || line.starts_with(' ') || line.starts_with('\t')
}

/// Detect blocks in diff lines. Returns blocks with placeholder IDs (0).
/// The caller should assign proper IDs and seq numbers.
pub fn detect_blocks(lines: &[String]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = &lines[i];
        if let Some((block_type, header)) = parse_block_header(line) {
            i += 1;
            let mut body = Vec::new();
            // Collect body: indented lines or empty lines, stop at non-indented content
            // or next block header
            while i < lines.len() {
                if parse_block_header(&lines[i]).is_some() {
                    break;
                }
                if !is_block_body_line(&lines[i]) {
                    break;
                }
                body.push(lines[i].clone());
                i += 1;
            }
            // Trim trailing empty lines from body
            while body.last().is_some_and(|l| l.trim().is_empty()) {
                body.pop();
            }
            blocks.push(Block {
                id: 0,
                block_type,
                header,
                body,
                seq: 0,
            });
        } else {
            i += 1;
        }
    }

    blocks
}

/// Extract lines not covered by any block.
pub fn extract_non_block_text(lines: &[String], _blocks: &[Block]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = &lines[i];
        if parse_block_header(line).is_some() {
            i += 1;
            // Skip body (same logic as detect_blocks)
            while i < lines.len() {
                if parse_block_header(&lines[i]).is_some() {
                    break;
                }
                if !is_block_body_line(&lines[i]) {
                    break;
                }
                i += 1;
            }
        } else {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                result.push(line.clone());
            }
            i += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_block_header_edited() {
        let result = parse_block_header("• Edited file.rs (+1 -1)");
        assert!(result.is_some());
        let (bt, text) = result.unwrap();
        assert_eq!(bt, "edited");
        assert_eq!(text, "Edited file.rs (+1 -1)");
    }

    #[test]
    fn test_parse_block_header_created() {
        let result = parse_block_header("• Created new_file.py");
        assert!(result.is_some());
        let (bt, _) = result.unwrap();
        assert_eq!(bt, "created");
    }

    #[test]
    fn test_parse_block_header_ran() {
        let result = parse_block_header("• Ran npm install");
        assert!(result.is_some());
        let (bt, _) = result.unwrap();
        assert_eq!(bt, "ran_command");
    }

    #[test]
    fn test_parse_block_header_read() {
        let result = parse_block_header("• Read config.toml");
        assert!(result.is_some());
        let (bt, _) = result.unwrap();
        assert_eq!(bt, "read");
    }

    #[test]
    fn test_parse_block_header_deleted() {
        let result = parse_block_header("• Deleted old.txt");
        assert!(result.is_some());
        let (bt, _) = result.unwrap();
        assert_eq!(bt, "deleted");
    }

    #[test]
    fn test_parse_block_header_none() {
        assert!(parse_block_header("Regular text line").is_none());
        assert!(parse_block_header("• Designing runtime...").is_none());
    }

    #[test]
    fn test_detect_edited_blocks() {
        let lines = vec![
            "• Edited PORTING_MATRIX.md (+2 -2)".into(),
            "    2".into(),
            "    3 -Reviewed: old".into(),
            "    3 +Reviewed: new".into(),
            "    4  Scope: every file".into(),
            "".into(),
            "• Edited UX_GAP_LOG.md (+2 -1)".into(),
            "    2".into(),
            "    3 -Reviewed: old".into(),
            "    3 +Reviewed: new".into(),
        ];
        let blocks = detect_blocks(&lines);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_type, "edited");
        assert_eq!(blocks[0].header, "Edited PORTING_MATRIX.md (+2 -2)");
        assert_eq!(blocks[0].body.len(), 4);
        assert_eq!(blocks[1].block_type, "edited");
        assert_eq!(blocks[1].body.len(), 3);
    }

    #[test]
    fn test_detect_ran_command_block() {
        let lines = vec![
            "• Ran cargo test".into(),
            "   Compiling myapp v0.1.0".into(),
            "   Running unittests".into(),
            "   test result: ok. 5 passed".into(),
        ];
        let blocks = detect_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, "ran_command");
        assert_eq!(blocks[0].body.len(), 3);
    }

    #[test]
    fn test_extract_non_block_text() {
        let lines = vec![
            "Analyzing the codebase...".into(),
            "• Edited file.rs (+1 -1)".into(),
            "    1 +new line".into(),
            "Continuing analysis...".into(),
        ];
        let blocks = detect_blocks(&lines);
        let remaining = extract_non_block_text(&lines, &blocks);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            remaining,
            vec!["Analyzing the codebase...", "Continuing analysis..."]
        );
    }
}
