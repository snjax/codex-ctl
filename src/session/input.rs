use std::os::fd::RawFd;
use std::time::Duration;

use anyhow::Result;

/// A key code that maps to terminal escape bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCode {
    Enter,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Space,
    CtrlC,
    CtrlD,
    CtrlZ,
    CtrlL,
}

impl KeyCode {
    pub fn to_bytes(&self) -> &[u8] {
        match self {
            KeyCode::Enter => b"\r",
            KeyCode::Tab => b"\t",
            KeyCode::Esc => b"\x1b",
            KeyCode::Up => b"\x1b[A",
            KeyCode::Down => b"\x1b[B",
            KeyCode::Right => b"\x1b[C",
            KeyCode::Left => b"\x1b[D",
            KeyCode::Backspace => b"\x7f",
            KeyCode::Space => b" ",
            KeyCode::CtrlC => b"\x03",
            KeyCode::CtrlD => b"\x04",
            KeyCode::CtrlZ => b"\x1a",
            KeyCode::CtrlL => b"\x0c",
        }
    }

    pub fn from_name(name: &str) -> Option<KeyCode> {
        match name.to_lowercase().as_str() {
            "enter" | "return" => Some(KeyCode::Enter),
            "tab" => Some(KeyCode::Tab),
            "esc" | "escape" => Some(KeyCode::Esc),
            "up" => Some(KeyCode::Up),
            "down" => Some(KeyCode::Down),
            "left" => Some(KeyCode::Left),
            "right" => Some(KeyCode::Right),
            "backspace" => Some(KeyCode::Backspace),
            "space" => Some(KeyCode::Space),
            "ctrl+c" => Some(KeyCode::CtrlC),
            "ctrl+d" => Some(KeyCode::CtrlD),
            "ctrl+z" => Some(KeyCode::CtrlZ),
            "ctrl+l" => Some(KeyCode::CtrlL),
            _ => None,
        }
    }
}

/// An action to perform on the PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Key(KeyCode),
    Text(String),
    Wait(Duration),
}

/// Parse a list of action arguments into Action values.
/// Reserved key names (case-insensitive) become Key actions.
/// "text:" prefix forces literal text.
/// "wait:N" becomes a Wait action (N in milliseconds).
/// Everything else is Text.
pub fn parse_actions(args: &[&str]) -> Vec<Action> {
    let mut actions = Vec::new();
    for arg in args {
        if let Some(text) = arg.strip_prefix("text:") {
            actions.push(Action::Text(text.to_string()));
        } else if let Some(ms_str) = arg.strip_prefix("wait:") {
            if let Ok(ms) = ms_str.parse::<u64>() {
                actions.push(Action::Wait(Duration::from_millis(ms)));
            } else {
                actions.push(Action::Text(arg.to_string()));
            }
        } else if let Some(key) = KeyCode::from_name(arg) {
            actions.push(Action::Key(key));
        } else {
            actions.push(Action::Text(arg.to_string()));
        }
    }
    actions
}

/// Execute a sequence of actions on a PTY master fd.
/// 30ms pause between actions. Text <50 chars sent whole,
/// >=50 chars chunked at 32 bytes with 10ms pause.
pub async fn execute_actions(master_fd: RawFd, actions: &[Action]) -> Result<()> {
    for (i, action) in actions.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        match action {
            Action::Key(key) => {
                write_to_pty(master_fd, key.to_bytes())?;
            }
            Action::Text(text) => {
                let bytes = text.as_bytes();
                if bytes.len() < 50 {
                    write_to_pty(master_fd, bytes)?;
                } else {
                    for chunk in bytes.chunks(32) {
                        write_to_pty(master_fd, chunk)?;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            Action::Wait(dur) => {
                tokio::time::sleep(*dur).await;
            }
        }
    }
    Ok(())
}

pub(crate) fn write_to_pty(fd: RawFd, data: &[u8]) -> Result<()> {
    use std::os::fd::BorrowedFd;
    // Safety: fd is a valid PTY master fd owned by the session
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    nix::unistd::write(borrowed, data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_actions_keys() {
        let args = vec!["down", "down", "enter"];
        let actions = parse_actions(&args);
        assert_eq!(
            actions,
            vec![
                Action::Key(KeyCode::Down),
                Action::Key(KeyCode::Down),
                Action::Key(KeyCode::Enter),
            ]
        );
    }

    #[test]
    fn test_parse_actions_mixed() {
        let args = vec!["fix the bug", "enter"];
        let actions = parse_actions(&args);
        assert_eq!(
            actions,
            vec![
                Action::Text("fix the bug".into()),
                Action::Key(KeyCode::Enter),
            ]
        );
    }

    #[test]
    fn test_parse_actions_wait() {
        let args = vec!["esc", "wait:500", "hello", "enter"];
        let actions = parse_actions(&args);
        assert_eq!(actions[0], Action::Key(KeyCode::Esc));
        assert_eq!(actions[1], Action::Wait(Duration::from_millis(500)));
        assert_eq!(actions[2], Action::Text("hello".into()));
        assert_eq!(actions[3], Action::Key(KeyCode::Enter));
    }

    #[test]
    fn test_parse_actions_text_escape() {
        let args = vec!["text:enter"];
        let actions = parse_actions(&args);
        assert_eq!(actions, vec![Action::Text("enter".into())]);
    }

    #[test]
    fn test_parse_actions_case_insensitive() {
        let args = vec!["ENTER", "Tab", "ESC"];
        let actions = parse_actions(&args);
        assert_eq!(
            actions,
            vec![
                Action::Key(KeyCode::Enter),
                Action::Key(KeyCode::Tab),
                Action::Key(KeyCode::Esc),
            ]
        );
    }

    #[test]
    fn test_keycode_to_bytes() {
        assert_eq!(KeyCode::Enter.to_bytes(), b"\r");
        assert_eq!(KeyCode::Esc.to_bytes(), b"\x1b");
        assert_eq!(KeyCode::Up.to_bytes(), b"\x1b[A");
        assert_eq!(KeyCode::CtrlC.to_bytes(), b"\x03");
    }
}
