use regex::Regex;
use std::sync::LazyLock;

/// Message types for classified output.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MsgType {
    AgentOutput,
    Status,
    StateChange,
    Prompt,
    Block,
    Raw,
}

impl MsgType {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            MsgType::AgentOutput => "agent_output",
            MsgType::Status => "status",
            MsgType::StateChange => "state_change",
            MsgType::Prompt => "prompt",
            MsgType::Block => "block",
            MsgType::Raw => "raw",
        }
    }
}

static TIMER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\d+[ms]\s+\d+[ms]\s*•").unwrap());

static SPINNER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏⣾⣽⣻⢿⡿⣟⣯⣷]").unwrap());

static TIMER_SIMPLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+m\s+\d+s").unwrap());

/// Classify a line of diff text into a message type.
pub fn classify_line(line: &str) -> MsgType {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return MsgType::Raw;
    }

    // Timer/spinner patterns → Status
    if TIMER_RE.is_match(trimmed)
        || SPINNER_RE.is_match(trimmed)
        || (TIMER_SIMPLE_RE.is_match(trimmed) && trimmed.contains("esc to interrupt"))
    {
        return MsgType::Status;
    }

    // Block headers are handled separately by blocks module
    // Question/prompt lines
    if trimmed.starts_with("Question ") {
        return MsgType::Prompt;
    }

    // Substantive text → AgentOutput
    MsgType::AgentOutput
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_status_timer() {
        let line = "• Designing runtime (1m 00s • esc to interrupt)";
        assert_eq!(classify_line(line), MsgType::Status);
    }

    #[test]
    fn test_classify_agent_output() {
        let line = "I'll analyze the codebase structure";
        assert_eq!(classify_line(line), MsgType::AgentOutput);
    }

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_line(""), MsgType::Raw);
        assert_eq!(classify_line("   "), MsgType::Raw);
    }

    #[test]
    fn test_classify_question() {
        let line = "Question 1/3 (3 unanswered)";
        assert_eq!(classify_line(line), MsgType::Prompt);
    }
}
