use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use anyhow::{Context, Result};
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::unistd::{Pid, execvp};

const PTY_ROWS: u16 = 500;
const PTY_COLS: u16 = 200;

/// Result of spawning a codex process under PTY.
pub struct SpawnResult {
    pub pid: Pid,
    pub master_fd: OwnedFd,
}

/// Find the codex binary path.
fn find_codex_binary() -> Result<String> {
    if let Ok(path) = std::env::var("CODEX_CTL_CODEX_PATH") {
        return Ok(path);
    }
    let path = which::which("codex")
        .context("Cannot find 'codex' in PATH. Set $CODEX_CTL_CODEX_PATH.")?;
    Ok(path.to_string_lossy().into_owned())
}

/// Spawn codex under a PTY with the given prompt and working directory.
///
/// When `resume_id` is `Some(id)`, spawns
/// `codex resume <id> --dangerously-bypass-approvals-and-sandbox --no-alt-screen -C <cwd> [prompt]`.
/// When `resume_id` is `None`, spawns
/// `codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen -C <cwd> <prompt>`.
///
/// Returns the child PID and the master PTY fd (parent side).
pub fn spawn_codex(prompt: Option<&str>, cwd: &Path, resume_id: Option<&str>) -> Result<SpawnResult> {
    let winsize = Winsize {
        ws_row: PTY_ROWS,
        ws_col: PTY_COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let codex_path = find_codex_binary()?;

    // Safety: forkpty is an unsafe FFI call that forks the process.
    // After fork, the child must only call async-signal-safe functions
    // before execvp.
    let fork_result = unsafe { forkpty(&winsize, None) }
        .context("forkpty failed")?;

    match fork_result {
        ForkptyResult::Parent { child, master } => {
            // Set master fd to non-blocking
            let fd = master.as_fd();
            use std::os::fd::AsRawFd;
            let raw_fd = fd.as_raw_fd();
            let flags = nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_GETFL)?;
            let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
            oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
            nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

            Ok(SpawnResult {
                pid: child,
                master_fd: master,
            })
        }
        ForkptyResult::Child => {
            // Child process — only sync, async-signal-safe calls here.
            // Set working directory
            if let Err(e) = std::env::set_current_dir(cwd) {
                eprintln!("codex-ctl: failed to chdir: {e}");
                std::process::exit(1);
            }

            let codex_c = CString::new(codex_path.as_str()).unwrap();
            let cwd_str = cwd.to_string_lossy();

            // Build args depending on resume mode
            let mut args: Vec<CString> = Vec::new();
            args.push(codex_c.clone());

            if let Some(id) = resume_id {
                // codex resume <id> --dangerously-bypass-approvals-and-sandbox --no-alt-screen -C <cwd> [prompt]
                args.push(CString::new("resume").unwrap());
                args.push(CString::new(id).unwrap());
            }

            args.push(CString::new("--dangerously-bypass-approvals-and-sandbox").unwrap());
            args.push(CString::new("--no-alt-screen").unwrap());
            args.push(CString::new("-C").unwrap());
            args.push(CString::new(cwd_str.as_ref()).unwrap());

            if let Some(p) = prompt {
                args.push(CString::new(p).unwrap());
            }

            // execvp replaces the process image
            let _ = execvp(&codex_c, &args);
            // If we get here, exec failed
            eprintln!("codex-ctl: failed to exec codex");
            std::process::exit(1);
        }
    }
}

/// Spawn an arbitrary command under PTY (for testing).
#[allow(dead_code)]
pub fn spawn_command(program: &str, args: &[&str], cwd: &Path) -> Result<SpawnResult> {
    let winsize = Winsize {
        ws_row: PTY_ROWS,
        ws_col: PTY_COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let fork_result = unsafe { forkpty(&winsize, None) }
        .context("forkpty failed")?;

    match fork_result {
        ForkptyResult::Parent { child, master } => {
            let fd = master.as_fd();
            use std::os::fd::AsRawFd;
            let raw_fd = fd.as_raw_fd();
            let flags = nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_GETFL)?;
            let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
            oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
            nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

            Ok(SpawnResult {
                pid: child,
                master_fd: master,
            })
        }
        ForkptyResult::Child => {
            if let Err(e) = std::env::set_current_dir(cwd) {
                eprintln!("codex-ctl: failed to chdir: {e}");
                std::process::exit(1);
            }

            let prog_c = CString::new(program).unwrap();
            let args_c: Vec<CString> = std::iter::once(prog_c.clone())
                .chain(args.iter().map(|a| CString::new(*a).unwrap()))
                .collect();

            let _ = execvp(&prog_c, &args_c);
            eprintln!("codex-ctl: failed to exec {program}");
            std::process::exit(1);
        }
    }
}
