//! Run real `ps` output through the level-1a decorator, for eyeballing against live data:
//!
//!   ps aux | cargo run --example decorate --manifest-path crates/ps-decorate/Cargo.toml
//!
//! Prints the decorated rows (dim zeros shown as `–`, sizes in K/M/G) and the kernel-fold
//! summary — the same model the `decorate_ps` bridge command hands the frontend. Uses the
//! tolerant `decorate_scrollback` path (the one the manual trigger uses on a buffer scrape).

use std::io::Read;

fn main() {
    let mut block = String::new();
    std::io::stdin()
        .read_to_string(&mut block)
        .expect("read stdin");

    match sampa_ps_decorate::decorate_scrollback(&block) {
        None => {
            eprintln!("no ps aux table found (raw passthrough)");
            std::process::exit(1);
        }
        Some(q) => {
            println!(
                "{:>7}  {:<8} {:>5} {:>5} {:>8}  {:<8} COMMAND",
                "PID", "USER", "%CPU", "%MEM", "RSS", "START"
            );
            for r in &q.rows {
                println!(
                    "{:>7}  {:<8} {:>5} {:>5} {:>8}  {:<8} {}",
                    r.pid, r.user, r.cpu, r.mem, r.rss, r.start, r.command
                );
            }
            if let Some(s) = q.kernel_summary() {
                println!("{s}");
            }
            eprintln!("\n[{} rows shown, {} kernel folded]", q.rows.len(), q.kernel_count);
        }
    }
}
