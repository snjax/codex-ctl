pub mod attach;

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Find a suitable terminal emulator.
pub fn find_terminal() -> Result<(String, Vec<String>)> {
    // Priority: $CODEX_CTL_TERMINAL → $TERMINAL → search PATH
    if let Ok(term) = std::env::var("CODEX_CTL_TERMINAL") {
        return Ok((term, vec!["-e".into()]));
    }
    if let Ok(term) = std::env::var("TERMINAL") {
        return Ok((term, vec!["-e".into()]));
    }

    let candidates = [
        ("foot", vec!["-e"]),
        ("alacritty", vec!["-e"]),
        ("kitty", vec!["--"]),
        ("xterm", vec!["-e"]),
        ("x-terminal-emulator", vec!["-e"]),
    ];

    for (name, flag) in &candidates {
        if which::which(name).is_ok() {
            return Ok((
                name.to_string(),
                flag.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }

    bail!("No terminal emulator found. Set $CODEX_CTL_TERMINAL or $TERMINAL, or install foot/alacritty/kitty/xterm.");
}

/// Spawn a GUI debug window for a session.
/// Returns the PID of the terminal process.
pub fn spawn_gui_window(session_id: &str, codex_ctl_binary: &Path) -> Result<u32> {
    let (terminal, exec_flag) = find_terminal()?;

    let mut cmd = Command::new(&terminal);
    for flag in &exec_flag {
        cmd.arg(flag);
    }
    cmd.arg(codex_ctl_binary);
    cmd.arg("_gui-attach");
    cmd.arg(session_id);

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn terminal: {terminal}"))?;

    Ok(child.id())
}
