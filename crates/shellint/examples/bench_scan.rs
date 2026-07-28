//! Throughput benchmark for the OSC scanner (Phase 5.1 / DESIGN.md §14).
//!
//! Every byte of PTY output flows through `OscScanner::feed` in the bridge's
//! output pump, so its throughput bounds how fast Sampa can absorb a flood. This
//! feeds a representative mix of plain text + periodic OSC 7/133 marks through the
//! scanner in 8 KiB chunks (matching the real reader) and reports MB/s.
//!
//! Run: `cargo run --release --manifest-path crates/shellint/Cargo.toml --example bench_scan`
//! Prints `scan: <N> MiB in <T> ms = <X> MiB/s`. Exits non-zero if throughput
//! falls under a lenient floor (catches catastrophic regressions, not noise).

use std::time::Instant;

use sampa_shellint::OscScanner;

const FLOOR_MIB_S: f64 = 30.0; // generous; real numbers are far higher

fn main() {
    // ~1 MiB block of representative terminal output: printable lines with an
    // occasional OSC 7 (cwd) and OSC 133 (prompt mark), like a real shell session.
    let mut block: Vec<u8> = Vec::with_capacity(1 << 20);
    let line = b"the quick brown fox jumps over the lazy dog 0123456789 \x1b[32mgreen\x1b[0m\n";
    while block.len() < (1 << 20) {
        block.extend_from_slice(line);
        if block.len() % 4096 < line.len() {
            block.extend_from_slice(b"\x1b]7;file:///home/user/project\x07");
            block.extend_from_slice(b"\x1b]133;A\x07prompt$ \x1b]133;B\x07");
        }
    }

    let total_bytes: u64 = 128 * block.len() as u64; // ~128 MiB
    let mut scanner = OscScanner::new();
    let mut events: u64 = 0;

    let start = Instant::now();
    for _ in 0..128 {
        for chunk in block.chunks(8192) {
            events += scanner.feed(chunk).len() as u64;
        }
    }
    let elapsed = start.elapsed();

    let mib = total_bytes as f64 / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64();
    let rate = mib / secs;
    // Use `events` so the optimizer can't elide the work.
    println!(
        "scan: {mib:.0} MiB in {ms:.0} ms = {rate:.0} MiB/s ({events} events)",
        ms = secs * 1000.0,
    );

    if rate < FLOOR_MIB_S {
        eprintln!("REGRESSION: scan throughput {rate:.0} MiB/s < floor {FLOOR_MIB_S:.0} MiB/s");
        std::process::exit(1);
    }
}
