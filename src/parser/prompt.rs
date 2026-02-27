use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static QUESTION_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Question\s+(\d+)/(\d+)").unwrap());

static OPTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*›?\s*(\d+)\.\s+(.+)$").unwrap());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInfo {
    pub question_num: u8,
    pub question_total: u8,
    pub question_text: String,
    pub options: Vec<PromptOption>,
    pub selected: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptOption {
    pub number: u8,
    pub label: String,
    pub description: String,
    pub is_selected: bool,
}

/// Parse the question header "Question N/M" from screen lines.
pub fn parse_question_header(lines: &[String]) -> Option<(u8, u8)> {
    for line in lines {
        if let Some(caps) = QUESTION_HEADER_RE.captures(line) {
            let num: u8 = caps[1].parse().ok()?;
            let total: u8 = caps[2].parse().ok()?;
            return Some((num, total));
        }
    }
    None
}

/// Parse prompt options from screen lines.
/// Detects the `›` marker for the selected option.
pub fn parse_options(lines: &[String]) -> Vec<PromptOption> {
    let mut options = Vec::new();

    for line in lines {
        let trimmed = line.trim_start();
        if let Some(caps) = OPTION_RE.captures(trimmed) {
            let number: u8 = match caps[1].parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let rest = caps[2].to_string();

            // Split label and description by multiple spaces
            let (label, description) = if let Some(pos) = rest.find("   ") {
                let l = rest[..pos].trim().to_string();
                let d = rest[pos..].trim().to_string();
                (l, d)
            } else {
                (rest.clone(), String::new())
            };

            let is_selected = line.trim_start().starts_with('›');

            options.push(PromptOption {
                number,
                label,
                description,
                is_selected,
            });
        }
    }

    options
}

/// Parse the full prompt from screen lines.
pub fn parse_prompt(lines: &[String]) -> Option<PromptInfo> {
    let (question_num, question_total) = parse_question_header(lines)?;

    let options = parse_options(lines);
    let selected = options
        .iter()
        .find(|o| o.is_selected)
        .map(|o| o.number)
        .unwrap_or(1);

    // Extract question text: lines between header and first option
    let mut question_text = String::new();
    let mut after_header = false;
    for line in lines {
        if QUESTION_HEADER_RE.is_match(line) {
            after_header = true;
            continue;
        }
        if after_header {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !question_text.is_empty() {
                    break;
                }
                continue;
            }
            if OPTION_RE.is_match(trimmed) || trimmed.starts_with('›') {
                break;
            }
            if !question_text.is_empty() {
                question_text.push(' ');
            }
            question_text.push_str(trimmed);
        }
    }

    Some(PromptInfo {
        question_num,
        question_total,
        question_text,
        options,
        selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_question_header() {
        let lines = vec![
            "Question 1/3 (3 unanswered)".into(),
            "Some text".into(),
        ];
        assert_eq!(parse_question_header(&lines), Some((1, 3)));
    }

    #[test]
    fn test_parse_question_header_none() {
        let lines = vec!["No question here".into()];
        assert_eq!(parse_question_header(&lines), None);
    }

    #[test]
    fn test_parse_options() {
        let lines = vec![
            "› 1. Productivity (Recommended)   More output on tasks.".into(),
            "  2. Balance                       Moderate pace.".into(),
            "  3. Recovery                      Reduce workload.".into(),
            "  4. None of the above             Add details in notes.".into(),
        ];
        let options = parse_options(&lines);
        assert_eq!(options.len(), 4);
        assert_eq!(options[0].number, 1);
        assert!(options[0].label.contains("Productivity"));
        assert!(options[0].is_selected);
        assert!(!options[1].is_selected);
        assert_eq!(options[1].number, 2);
    }

    #[test]
    fn test_parse_prompt_full() {
        let lines = vec![
            "Question 1/3 (3 unanswered)".into(),
            "Choose a direction:".into(),
            "".into(),
            "› 1. Option A   Description A".into(),
            "  2. Option B   Description B".into(),
            "  3. Option C   Description C".into(),
            "".into(),
            "tab to add notes | enter to submit answer".into(),
        ];
        let prompt = parse_prompt(&lines).unwrap();
        assert_eq!(prompt.question_num, 1);
        assert_eq!(prompt.question_total, 3);
        assert_eq!(prompt.question_text, "Choose a direction:");
        assert_eq!(prompt.options.len(), 3);
        assert_eq!(prompt.selected, 1);
    }
}
