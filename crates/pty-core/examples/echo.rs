//! Smoke test for the PTY layer with no GUI: spawn the shell, run a command,
//! print what comes back. This is the M0 proof that Layer 1 works.
//!
//!   cargo run --manifest-path crates/pty-core/Cargo.toml --example echo

use std::sync::mpsc::channel;
use std::time::Duration;

use pty_core::{spawn, PtyEvent, SpawnConfig};

fn main() -> anyhow::Result<()> {
    let (tx, rx) = channel::<PtyEvent>();

    let mut pty = spawn(
        SpawnConfig {
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
            args: vec![],
            cwd: std::env::var("HOME").ok(),
            cols: 80,
            rows: 24,
            env: vec![],
        },
        tx,
    )?;

    eprintln!("[spawned shell, pid {:?}]", pty.pid());

    // Run a command and exit so the stream closes cleanly.
    pty.write(b"echo \"hello from $ZSH_NAME ${ZSH_VERSION:-$0}\"; exit\n")?;

    while let Ok(event) = rx.recv_timeout(Duration::from_secs(3)) {
        match event {
            PtyEvent::Output(bytes) => print!("{}", String::from_utf8_lossy(&bytes)),
            PtyEvent::Exit(info) => {
                eprintln!("\n[session ended: {} (code {})]", info.detail, info.code);
                break;
            }
        }
    }
    Ok(())
}
