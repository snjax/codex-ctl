/// Take a snapshot of the terminal screen as a Vec of lines.
pub fn take_snapshot(parser: &vt100::Parser) -> Vec<String> {
    let screen = parser.screen();
    let cols = screen.size().1;
    // rows(start_col, width) returns an iterator over ALL visible rows,
    // each showing text from column start_col with the given width.
    screen.rows(0, cols).collect()
}

/// Filter screen lines: trim trailing whitespace, collapse 3+ blank lines to 2,
/// trim trailing blank lines.
pub fn filter_lines(lines: &[String]) -> Vec<String> {
    // Trim trailing whitespace on each line
    let trimmed: Vec<String> = lines.iter().map(|l| l.trim_end().to_string()).collect();

    // Collapse 3+ consecutive blank lines to 2
    let mut result: Vec<String> = Vec::new();
    let mut consecutive_blanks = 0;

    for line in &trimmed {
        if line.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 2 {
                result.push(line.clone());
            }
        } else {
            consecutive_blanks = 0;
            result.push(line.clone());
        }
    }

    // Trim trailing blank lines
    while result.last().is_some_and(|l| l.is_empty()) {
        result.pop();
    }

    result
}

/// Strip UI chrome (top frame, bottom status/help, scattered noise) from screen lines.
/// Keeps only conversation content (user prompts, agent messages, blocks).
pub fn strip_ui_chrome(lines: &[String]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }

    // Phase 1: Top stripping — skip frame chars (╭│╰), Tip:, and empty lines between them
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('╭')
            || trimmed.starts_with('│')
            || trimmed.starts_with('╰')
            || trimmed.starts_with("Tip:")
        {
            start = i + 1;
        } else {
            break;
        }
    }

    // Phase 2: Bottom stripping — find "? for shortcuts" or "context left" line
    let mut end = lines.len();
    for i in (start..lines.len()).rev() {
        let trimmed = lines[i].trim();
        if trimmed.contains("? for shortcuts") || trimmed.contains("context left") {
            end = i;
            break;
        }
    }

    // Strip trailing suggestion prompts (› lines) and empty lines above help bar
    while end > start {
        let trimmed = lines[end - 1].trim();
        if trimmed.is_empty() || trimmed.starts_with('›') {
            end -= 1;
        } else {
            break;
        }
    }

    if start >= end {
        return Vec::new();
    }

    // Phase 3: Scattered noise — filter out transient status lines and decorative rules
    lines[start..end]
        .iter()
        .filter(|l| {
            let t = l.trim();
            // Transient timer/status
            if l.contains("esc to interrupt") || l.contains("esc again to edit") {
                return false;
            }
            // Horizontal rules (only ─ chars)
            if !t.is_empty() && t.chars().all(|c| c == '─') {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

/// Compute diff between previous and next screen snapshots.
/// Returns only new/changed lines.
pub fn compute_diff(prev: &[String], next: &[String]) -> Vec<String> {
    let mut diff = Vec::new();

    for (i, line) in next.iter().enumerate() {
        if i >= prev.len() {
            // New line beyond previous length
            if !line.is_empty() {
                diff.push(line.clone());
            }
        } else if prev[i] != *line && !line.is_empty() {
            // Changed line
            diff.push(line.clone());
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_lines_basic() {
        let lines = vec![
            "Hello world                        ".into(),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            "Next line".into(),
            "".into(),
        ];
        let result = filter_lines(&lines);
        assert_eq!(result, vec!["Hello world", "", "", "Next line"]);
    }

    #[test]
    fn test_filter_lines_no_blanks() {
        let lines = vec!["line1".into(), "line2".into(), "line3".into()];
        let result = filter_lines(&lines);
        assert_eq!(result, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_filter_lines_all_blank() {
        let lines = vec!["".into(), "".into(), "".into(), "".into()];
        let result = filter_lines(&lines);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_diff_new_lines() {
        let prev = vec!["line1".into(), "line2".into()];
        let next = vec!["line1".into(), "line2".into(), "line3 new".into()];
        let diff = compute_diff(&prev, &next);
        assert_eq!(diff, vec!["line3 new"]);
    }

    #[test]
    fn test_compute_diff_changed_line() {
        let prev = vec!["line1".into(), "line2".into()];
        let next = vec!["line1".into(), "line2 changed".into()];
        let diff = compute_diff(&prev, &next);
        assert_eq!(diff, vec!["line2 changed"]);
    }

    #[test]
    fn test_compute_diff_no_change() {
        let prev = vec!["line1".into(), "line2".into()];
        let next = vec!["line1".into(), "line2".into()];
        let diff = compute_diff(&prev, &next);
        assert!(diff.is_empty());
    }

    // --- strip_ui_chrome tests ---

    #[test]
    fn test_strip_ui_chrome_full_screen() {
        let lines: Vec<String> = vec![
            "╭──────────────────────────────────────╮".into(),
            "│ codex GPT-4.1 (full auto)            │".into(),
            "╰──────────────────────────────────────╯".into(),
            "Tip: use /help for commands".into(),
            "".into(),
            "› Build a REST API".into(),
            "".into(),
            "• Sure, I'll build the API.".into(),
            "".into(),
            "### Edited src/main.rs".into(),
            "  +fn main() {}".into(),
            "".into(),
            "• Working 5s • esc to interrupt".into(),
            "────────────────────────────────".into(),
            "".into(),
            "› Write tests for the API".into(),
            "? for shortcuts | model: o4-mini | 85% context left".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec![
            "› Build a REST API",
            "",
            "• Sure, I'll build the API.",
            "",
            "### Edited src/main.rs",
            "  +fn main() {}",
            "",
            // "esc to interrupt" removed, horizontal rule removed,
            // suggestion prompt and help line removed
        ]);
    }

    #[test]
    fn test_strip_ui_chrome_no_frame() {
        let lines: Vec<String> = vec![
            "› Hello".into(),
            "• Response here".into(),
            "? for shortcuts | 100% context left".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec![
            "› Hello",
            "• Response here",
        ]);
    }

    #[test]
    fn test_strip_ui_chrome_no_help_line() {
        let lines: Vec<String> = vec![
            "╭───╮".into(),
            "│ codex │".into(),
            "╰───╯".into(),
            "› Prompt".into(),
            "• Agent output".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec!["› Prompt", "• Agent output"]);
    }

    #[test]
    fn test_strip_ui_chrome_empty_input() {
        let result = strip_ui_chrome(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_strip_ui_chrome_only_chrome() {
        let lines: Vec<String> = vec![
            "╭───╮".into(),
            "│ codex │".into(),
            "╰───╯".into(),
            "? for shortcuts | 100% context left".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert!(result.is_empty());
    }

    #[test]
    fn test_strip_ui_chrome_esc_again_to_edit() {
        let lines: Vec<String> = vec![
            "› Hello".into(),
            "• Working 3s • esc again to edit".into(),
            "• Real output here".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec!["› Hello", "• Real output here"]);
    }

    #[test]
    fn test_strip_ui_chrome_suggestion_above_help() {
        let lines: Vec<String> = vec![
            "• Agent said something".into(),
            "› Try running cargo test".into(),
            "? for shortcuts | 50% context left".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec!["• Agent said something"]);
    }

    #[test]
    fn test_strip_ui_chrome_multiple_suggestions_above_help() {
        let lines: Vec<String> = vec![
            "• Output here".into(),
            "".into(),
            "› Suggestion one".into(),
            "› Suggestion two".into(),
            "? for shortcuts | 50% context left".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec!["• Output here"]);
    }

    #[test]
    fn test_strip_ui_chrome_horizontal_rule() {
        let lines: Vec<String> = vec![
            "› Hello".into(),
            "────────────────────────".into(),
            "• Response".into(),
        ];
        let result = strip_ui_chrome(&lines);
        assert_eq!(result, vec!["› Hello", "• Response"]);
    }
}
