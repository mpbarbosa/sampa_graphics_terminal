//! Parse `aws help` into the command-name list for the aws helper panel.
//!
//! Like the other CLI cheat-sheets, when the user has typed `aws` and presses the man-panel
//! shortcut, Sampa shows this list instead of a man page. The bridge runs `aws <path…> help`
//! and hands its output here; `parse_aws_help` returns the service/command names under the
//! `AVAILABLE SERVICES` / `AVAILABLE COMMANDS` header, in order.
//!
//! aws differs from the other CLIs in two ways (hence a separate core):
//! - Its help is a **groff-rendered man page** — the text carries ANSI SGR sequences and/or
//!   backspace-overstrike bold/underline, which this core strips first (see [`strip`]).
//! - It lists names as **`o <name>` bullets** (one per line, blank lines between) with **no
//!   per-command descriptions**, so this returns names only — the frontend lays them in columns.
//!
//! (The command form `aws <path…> help` — a `help` pseudo-subcommand, not `--help` — is the
//! bridge's concern.) Pure — `std` only, **no shell, no Tauri**. Fails safe: output with no
//! `AVAILABLE …` bullet list yields `None` (a leaf like `aws s3 ls help`), mirroring the others.

/// Strip groff man-page decoration so the text can be parsed / shown as plain text: ANSI CSI
/// escape sequences (`ESC [ … <final>`) and backspace-overstrike (`X\x08Y` → `Y`, used for bold
/// `a\x08a` and underline `_\x08a`). Other control bytes (except tab/newline) are dropped too.
pub fn strip(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // ANSI escape: `ESC [ params… final`. Skip the introducer and params up to the
                // final byte (@-~). A bare/`ESC(` etc. just drops the next char defensively.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&p) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\u{8}' => {
                // Backspace-overstrike: the previous emitted char is overwritten by the next.
                out.pop();
            }
            '\n' | '\t' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {} // other C0 / DEL
            c => out.push(c),
        }
    }
    out
}

/// Parse `aws help` output into its service/command names. After stripping man-page decoration,
/// an `AVAILABLE SERVICES` / `AVAILABLE COMMANDS` header (a non-indented line mentioning
/// "available" + "service"/"command") opens the section; the indented `o <name>` bullets that
/// follow are collected until the next non-indented header. `None` if no names are found.
pub fn parse_aws_help(output: &str) -> Option<Vec<String>> {
    let clean = strip(output);
    let mut in_section = false;
    let mut out = Vec::new();
    for line in clean.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            let h = line.trim().to_ascii_lowercase();
            in_section = h.contains("available")
                && (h.contains("service") || h.contains("command") || h.contains("subcommand"));
            continue;
        }
        if !in_section {
            continue;
        }
        // Inside the section: `o <name>` bullets (blank lines between). Skip anything else.
        if let Some(name) = line.trim().strip_prefix("o ") {
            let name = name.trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.push(name.to_string());
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed `aws help` sample with ANSI SGR on the header (as groff emits).
    const SAMPLE: &str = "\u{1b}[1mNAME\u{1b}[0m
       aws -

\u{1b}[1mAVAILABLE SERVICES\u{1b}[0m
       o accessanalyzer

       o account

       o acm-pca

       o dynamodb

       o s3

\u{1b}[1mSEE ALSO\u{1b}[0m
       o something-else
";

    #[test]
    fn parses_service_names_stripping_ansi() {
        let names = parse_aws_help(SAMPLE).unwrap();
        assert_eq!(names, vec!["accessanalyzer", "account", "acm-pca", "dynamodb", "s3"]);
        // SEE ALSO is not an AVAILABLE section, so its bullet is excluded.
        assert!(!names.contains(&"something-else".to_string()));
    }

    #[test]
    fn parses_subcommand_available_commands() {
        // `aws s3 help` uses `AVAILABLE COMMANDS`; here with backspace-overstrike bold.
        let out = "A\u{8}AV\u{8}VA\u{8}AI\u{8}IL\u{8}LA\u{8}AB\u{8}BL\u{8}LE\u{8}E COMMANDS
       o cp

       o ls

       o mb
";
        let names = parse_aws_help(out).unwrap();
        assert_eq!(names, vec!["cp", "ls", "mb"]);
    }

    #[test]
    fn leaf_and_non_aws_are_none() {
        // `aws s3 ls help` — a leaf with only OPTIONS/EXAMPLES, no AVAILABLE section.
        let out = "\u{1b}[1mDESCRIPTION\u{1b}[0m
       List S3 objects.

\u{1b}[1mOPTIONS\u{1b}[0m
       o --recursive
";
        assert!(parse_aws_help(out).is_none());
        assert!(parse_aws_help("").is_none());
        assert!(parse_aws_help("just some text\n").is_none());
    }

    #[test]
    fn strip_handles_ansi_and_overstrike() {
        assert_eq!(strip("\u{1b}[1mBOLD\u{1b}[0m"), "BOLD");
        assert_eq!(strip("a\u{8}a b\u{8}b"), "a b"); // overstrike bold
        assert_eq!(strip("_\u{8}u"), "u"); // underline
    }
}
