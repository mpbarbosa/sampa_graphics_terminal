//! PTY read-pump throughput + flood-stability benchmark (Phase 5.1 / DESIGN.md §14).
//!
//! Spawns a shell that floods the PTY with a fixed volume and drains the event
//! channel to EOF, measuring how fast the reader thread + channel move bytes and
//! confirming the pump terminates cleanly under a flood (no hang, no unbounded
//! growth — we hold at most one chunk at a time).
//!
//! Run: `cargo run --release --manifest-path crates/pty-core/Cargo.toml --example bench_pump`
//! Prints `pump: <N> MiB in <T> ms = <X> MiB/s`. Exits non-zero on a stall or if
//! throughput falls under a lenient floor.

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use pty_core::{spawn, PtyEvent, SpawnConfig};

const FLOOD_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
const FLOOR_MIB_S: f64 = 20.0;
const STALL_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    // `head -c N /dev/zero` emits exactly N bytes as fast as the PTY will take
    // them — a pure producer, so we measure our read/channel path, not the shell.
    let cfg = SpawnConfig {
        shell: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            format!("head -c {FLOOD_BYTES} /dev/zero"),
        ],
        cwd: None,
        cols: 80,
        rows: 25,
        env: vec![],
    };

    let (tx, rx) = channel();
    let _handle = spawn(cfg, tx).expect("spawn shell");

    let mut bytes: u64 = 0;
    let start = Instant::now();
    loop {
        match rx.recv_timeout(STALL_TIMEOUT) {
            Ok(PtyEvent::Output(chunk)) => bytes += chunk.len() as u64,
            Ok(PtyEvent::Exit(_)) => break,
            Err(RecvTimeoutError::Timeout) => {
                eprintln!("STALL: no PTY event for {}s — pump hung", STALL_TIMEOUT.as_secs());
                std::process::exit(1);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let elapsed = start.elapsed();

    let mib = bytes as f64 / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64();
    let rate = mib / secs;
    println!("pump: {mib:.0} MiB in {ms:.0} ms = {rate:.0} MiB/s", ms = secs * 1000.0);

    // A tty may drop bytes it can't process (OPOST etc.); require most got through.
    let expected_mib = FLOOD_BYTES as f64 / (1024.0 * 1024.0);
    if mib < expected_mib * 0.5 {
        eprintln!("REGRESSION: only {mib:.0} of {expected_mib:.0} MiB drained");
        std::process::exit(1);
    }
    if rate < FLOOR_MIB_S {
        eprintln!("REGRESSION: pump throughput {rate:.0} MiB/s < floor {FLOOR_MIB_S:.0} MiB/s");
        std::process::exit(1);
    }
}
