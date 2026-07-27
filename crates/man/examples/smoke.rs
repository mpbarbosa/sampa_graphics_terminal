fn main() {
    for cmd in ["ls", "grep", "for", "definitely-not-a-command-xyz"] {
        match sampa_man::render(cmd) {
            Ok(Some(t)) => println!("{cmd}: Some ({} chars) — first line: {:?}", t.len(), t.lines().find(|l| !l.trim().is_empty())),
            Ok(None) => println!("{cmd}: None (no page / invalid)"),
            Err(e) => println!("{cmd}: Err {e}"),
        }
    }
}
