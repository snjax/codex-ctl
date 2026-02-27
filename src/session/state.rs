use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::parser::prompt::{self, PromptInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Working,
    Idle,
    Prompting,
    PromptingNotes,
    Dead,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionState::Working => write!(f, "working"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Prompting => write!(f, "prompting"),
            SessionState::PromptingNotes => write!(f, "prompting_notes"),
            SessionState::Dead => write!(f, "dead"),
        }
    }
}

impl SessionState {
    pub fn from_str_loose(s: &str) -> Option<SessionState> {
        match s.to_lowercase().as_str() {
            "working" => Some(SessionState::Working),
            "idle" => Some(SessionState::Idle),
            "prompting" => Some(SessionState::Prompting),
            "prompting_notes" => Some(SessionState::PromptingNotes),
            "dead" => Some(SessionState::Dead),
            _ => None,
        }
    }
}

/// Result of state detection including optional prompt info.
#[derive(Debug, Clone)]
pub struct DetectedState {
    pub state: SessionState,
    pub prompt_info: Option<PromptInfo>,
}

/// Detect the current session state from screen lines.
///
/// `last_esc_seen` is the last time "esc to interrupt" was observed on screen.
/// `now` is the current instant.
///
/// State detection runs on EVERY PTY read (no debounce).
pub fn detect_state(
    screen_lines: &[String],
    last_esc_seen: Instant,
    now: Instant,
) -> DetectedState {
    let has_esc_to_interrupt = screen_lines
        .iter()
        .any(|line| line.contains("esc to interrupt"));

    let has_clear_notes = screen_lines
        .iter()
        .any(|line| line.contains("tab or esc to clear notes"));

    let has_enter_submit = screen_lines
        .iter()
        .any(|line| line.contains("enter to submit"));

    let has_question = screen_lines
        .iter()
        .any(|line| line.starts_with("Question ") || line.contains("Question "));

    // Priority 1: PromptingNotes
    if has_clear_notes && has_question {
        return DetectedState {
            state: SessionState::PromptingNotes,
            prompt_info: prompt::parse_prompt(screen_lines),
        };
    }

    // Priority 2: Prompting
    if has_question && has_enter_submit {
        return DetectedState {
            state: SessionState::Prompting,
            prompt_info: prompt::parse_prompt(screen_lines),
        };
    }

    // Priority 3: Working (esc to interrupt present)
    if has_esc_to_interrupt {
        return DetectedState {
            state: SessionState::Working,
            prompt_info: None,
        };
    }

    // Priority 4: Idle (esc to interrupt absent for >= 1s)
    let elapsed = now.duration_since(last_esc_seen);
    if elapsed.as_secs_f64() >= 1.0 {
        return DetectedState {
            state: SessionState::Idle,
            prompt_info: None,
        };
    }

    // Grace period: still consider Working
    DetectedState {
        state: SessionState::Working,
        prompt_info: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_detect_state_working() {
        let now = Instant::now();
        let screen = vec![
            "• Designing runtime workspace resolution (1m 00s • esc to interrupt)".into(),
        ];
        let result = detect_state(&screen, now, now);
        assert_eq!(result.state, SessionState::Working);
    }

    #[test]
    fn test_detect_state_idle() {
        let now = Instant::now();
        let far_past = now - Duration::from_secs(5);
        let screen = vec!["Done. Waiting for input...".into()];
        let result = detect_state(&screen, far_past, now);
        assert_eq!(result.state, SessionState::Idle);
    }

    #[test]
    fn test_detect_state_working_grace_period() {
        let now = Instant::now();
        // esc was seen 200ms ago — still in grace period
        let recent = now - Duration::from_millis(200);
        let screen = vec!["Some output without esc marker".into()];
        let result = detect_state(&screen, recent, now);
        assert_eq!(result.state, SessionState::Working);
    }

    #[test]
    fn test_detect_state_prompting() {
        let now = Instant::now();
        let far_past = now - Duration::from_secs(5);
        let screen = vec![
            "Question 1/3 (3 unanswered)".into(),
            "Choose a direction:".into(),
            "".into(),
            "› 1. Option A   Description A".into(),
            "  2. Option B   Description B".into(),
            "".into(),
            "tab to add notes | enter to submit answer | esc to interrupt".into(),
        ];
        let result = detect_state(&screen, far_past, now);
        assert_eq!(result.state, SessionState::Prompting);
    }

    #[test]
    fn test_detect_state_prompting_notes() {
        let now = Instant::now();
        let far_past = now - Duration::from_secs(5);
        let screen = vec![
            "Question 1/3 (3 unanswered)".into(),
            "...".into(),
            "› Here is note...".into(),
            "tab or esc to clear notes | enter to submit answer".into(),
        ];
        let result = detect_state(&screen, far_past, now);
        assert_eq!(result.state, SessionState::PromptingNotes);
    }
}
