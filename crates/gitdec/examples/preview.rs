//! Eyeball the parser on live output: `git status --porcelain -b | cargo run --example preview`.
//! Prints the grouped model the frontend renders, or the fail-safe `None`.

use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("read stdin");
    match sampa_gitdec::parse_status(&input) {
        None => println!("(not porcelain output — the caller would show the raw error)"),
        Some(s) => {
            print!("branch {}", s.branch);
            if let Some(up) = &s.upstream {
                print!(" -> {up}");
            }
            println!(" [ahead {} behind {}]", s.ahead, s.behind);
            let group = |name: &str, cs: &[sampa_gitdec::Change]| {
                for c in cs {
                    let orig = c.orig.as_deref().map(|o| format!(" <- {o}")).unwrap_or_default();
                    println!("  {name:<10} {:<14} {}{orig}", c.label, c.path);
                }
            };
            group("conflict", &s.conflicted);
            group("staged", &s.staged);
            group("unstaged", &s.unstaged);
            group("untracked", &s.untracked);
            if s.is_clean() {
                println!("  (clean)");
            }
        }
    }
}
