///! Quick test: spawn dummy_codex.py under PTY and snapshot VT100 screen at intervals.
///! Run with: cargo run --example test_pty_capture
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let dummy_path = cwd.join("examples/dummy_simple.py");

    println!("=== Spawning dummy_codex.py (slow scenario) under PTY ===\n");

    let winsize = nix::pty::Winsize {
        ws_row: 50,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let fork_result = unsafe { nix::pty::forkpty(&winsize, None) }?;

    match fork_result {
        nix::pty::ForkptyResult::Parent { child, master } => {
            // Set non-blocking
            let fd = master.as_fd();
            let raw_fd = fd.as_raw_fd();
            let flags = nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_GETFL)?;
            let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
            oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
            nix::fcntl::fcntl(raw_fd, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

            let mut parser = vt100::Parser::new(50, 120, 0);
            let mut buf = [0u8; 4096];
            let start = Instant::now();
            let mut snapshot_count = 0;

            loop {
                if start.elapsed() > Duration::from_secs(12) {
                    println!("\n=== Timeout reached ===");
                    break;
                }

                // Try to read
                match nix::unistd::read(raw_fd, &mut buf) {
                    Ok(0) => {
                        println!("\n=== EOF ===");
                        break;
                    }
                    Ok(n) => {
                        parser.process(&buf[..n]);
                    }
                    Err(nix::errno::Errno::EAGAIN) => {
                        // No data available
                    }
                    Err(nix::errno::Errno::EIO) => {
                        println!("\n=== EIO (child exited) ===");
                        break;
                    }
                    Err(e) => {
                        eprintln!("Read error: {e}");
                        break;
                    }
                }

                // Snapshot every 500ms
                std::thread::sleep(Duration::from_millis(100));
                let elapsed = start.elapsed();
                if elapsed.as_millis() / 500 > snapshot_count {
                    snapshot_count = elapsed.as_millis() / 500;
                    println!("--- Snapshot at {:.1}s ---", elapsed.as_secs_f64());

                    let screen = parser.screen();
                    let cols = screen.size().1;
                    for (row, line) in screen.rows(0, cols).enumerate() {
                        let trimmed = line.trim_end();
                        if !trimmed.is_empty() {
                            println!("  row {:2}: {}", row, trimmed);
                        }
                    }
                    println!();
                }
            }

            // Final snapshot
            println!("--- Final snapshot ---");
            let screen = parser.screen();
            // Method 1: rows(0, cols) - FIXED
            println!("[rows() method - fixed]");
            let cols = screen.size().1;
            for (row, line) in screen.rows(0, cols).enumerate() {
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    println!("  row {:2}: {:?}", row, trimmed);
                }
            }
            // Method 2: contents()
            println!("\n[contents() method]");
            let contents = screen.contents();
            for (i, line) in contents.lines().enumerate() {
                if !line.trim().is_empty() {
                    println!("  line {:2}: {:?}", i, line);
                }
            }
            // Method 3: cell-by-cell for row 0
            println!("\n[cell-by-cell row 0, first 30 cols]");
            for col in 0..30u16 {
                let cell = screen.cell(0, col);
                if let Some(cell) = cell {
                    let ch = cell.contents();
                    if !ch.is_empty() && ch != " " {
                        print!("({},{})={:?} ", 0, col, ch);
                    }
                }
            }
            println!();

            // Reap child
            let _ = nix::sys::wait::waitpid(child, None);
        }
        nix::pty::ForkptyResult::Child => {
            let _ = std::env::set_current_dir(&cwd);
            let prog = std::ffi::CString::new("python3").unwrap();
            let args = [
                prog.clone(),
                std::ffi::CString::new(dummy_path.to_str().unwrap()).unwrap(),
                // std::ffi::CString::new("--scenario").unwrap(),
                // std::ffi::CString::new("slow").unwrap(),
            ];
            let _ = nix::unistd::execvp(&prog, &args);
            eprintln!("exec failed");
            std::process::exit(1);
        }
    }

    Ok(())
}
