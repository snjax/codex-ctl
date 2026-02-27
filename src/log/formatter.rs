use super::LogMessage;

/// Format log messages as markdown-like text output.
/// Returns lines of formatted text.
pub fn format_messages(messages: &[LogMessage]) -> Vec<String> {
    let mut lines = Vec::new();

    for msg in messages {
        match msg.msg_type.as_str() {
            "agent_output" => {
                lines.push(msg.text.clone());
            }
            "block" => {
                lines.push(format!("### {}", msg.text));
            }
            "status" => {
                lines.push(msg.text.clone());
            }
            "state_change" => {
                // Minimal: one-liner or skip
                if let (Some(from), Some(to)) = (&msg.state_from, &msg.state_to) {
                    lines.push(format!("[state: {from} → {to}]"));
                }
            }
            "prompt" => {
                if let Some(info) = &msg.prompt {
                    lines.push(format!(
                        "[prompt: Question {}/{} - {}]",
                        info.question_num, info.question_total, info.question_text
                    ));
                } else {
                    lines.push(format!("[prompt: {}]", msg.text));
                }
            }
            _ => {
                if !msg.text.is_empty() {
                    lines.push(msg.text.clone());
                }
            }
        }
    }

    lines
}

/// Build the JSON status footer line.
pub fn format_footer(
    state: &str,
    seq: u64,
    waited: bool,
    waited_sec: Option<f64>,
    timed_out: Option<bool>,
) -> serde_json::Value {
    let mut footer = serde_json::json!({
        "state": state,
        "seq": seq,
    });

    if waited {
        footer["waited"] = serde_json::json!(true);
        if let Some(ws) = waited_sec {
            footer["waited_sec"] = serde_json::json!(ws);
        }
        if let Some(to) = timed_out {
            footer["timed_out"] = serde_json::json!(to);
        }
    }

    footer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_agent_output() {
        let messages = vec![LogMessage::agent_output(1, "Hello world".into())];
        let lines = format_messages(&messages);
        assert_eq!(lines, vec!["Hello world"]);
    }

    #[test]
    fn test_format_block() {
        let messages = vec![LogMessage::block(
            1,
            "Edited file.rs (+2 -1)",
            1,
            "edited",
            5,
        )];
        let lines = format_messages(&messages);
        assert_eq!(lines, vec!["### Edited file.rs (+2 -1)"]);
    }

    #[test]
    fn test_format_state_change() {
        let messages = vec![LogMessage::state_change(1, "working", "idle")];
        let lines = format_messages(&messages);
        assert_eq!(lines, vec!["[state: working → idle]"]);
    }

    #[test]
    fn test_format_footer_basic() {
        let footer = format_footer("idle", 42, false, None, None);
        assert_eq!(footer["state"], "idle");
        assert_eq!(footer["seq"], 42);
    }

    #[test]
    fn test_format_footer_with_wait() {
        let footer = format_footer("idle", 42, true, Some(12.3), Some(false));
        assert_eq!(footer["waited"], true);
        assert_eq!(footer["waited_sec"], 12.3);
        assert_eq!(footer["timed_out"], false);
    }
}
