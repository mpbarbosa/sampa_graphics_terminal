//! Live man-page rendering (DESIGN.md §10.2).
//!
//! The command name is **validated** before it ever reaches `man`, `man` is invoked
//! with an **argument vector (no shell)**, and its output is **sanitized** of the
//! nroff overstrike and ANSI formatting that `man -P cat` emits. Validation and
//! sanitizing are pure and unit-tested; only [`render`] touches the process.

/// Accept a command name for use as a `man` argument: `^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$`.
/// This is defence-in-depth on top of the no-shell arg vector — it rejects anything
/// that isn't plausibly a command (paths, options, shell metacharacters, spaces).
pub fn valid_command(cmd: &str) -> bool {
    let b = cmd.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() {
        return false;
    }
    b[1..]
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'+' | b'-'))
}

/// Strip `man`/nroff formatting to plain text: ANSI SGR sequences first, then the
/// backspace-overstrike used for bold (`X\bX`) and underline (`_\bX`).
pub fn sanitize(raw: &str) -> String {
    collapse_overstrike(&strip_ansi(raw))
}

/// Remove ANSI/OSC escape sequences (`ESC [ … final`, `ESC ] … BEL/ST`).
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI: skip until a final byte in 0x40..=0x7e.
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1; // consume the final byte
                }
                b']' => {
                    // OSC: skip until BEL or ST (ESC \).
                    i += 2;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                _ => {
                    i += 2; // other two-byte escape
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Collapse backspace overstrike: a `\x08` erases the previously emitted char, so the
/// following char overwrites it (`X\bX` → `X`, `_\bX` → `X`).
fn collapse_overstrike(s: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\u{8}' {
            out.pop();
        } else {
            out.push(ch);
        }
    }
    out.into_iter().collect()
}

/// Render `man <cmd>` as sanitized plain text. Returns `Ok(None)` when `cmd` is
/// invalid, has no man page, or the page is empty. `man` is run with an arg vector
/// (never a shell) and a fixed width.
pub fn render(cmd: &str) -> std::io::Result<Option<String>> {
    if !valid_command(cmd) {
        return Ok(None);
    }
    let output = std::process::Command::new("man")
        .args(["-P", "cat", cmd])
        .env("MANWIDTH", "80")
        .env("MAN_KEEP_FORMATTING", "1")
        .output()?;
    if !output.status.success() {
        return Ok(None); // no such page
    }
    let text = sanitize(&String::from_utf8_lossy(&output.stdout));
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_command_accepts_real_names() {
        for ok in ["grep", "git", "ls", "7z", "gcc-12", "a.out", "lib_foo", "g++"] {
            assert!(valid_command(ok), "{ok} should be valid");
        }
    }

    #[test]
    fn valid_command_rejects_injection_and_junk() {
        for bad in [
            "", "rm -rf", "foo;bar", "a|b", "-x", "../etc/passwd", "$(x)", "a b",
            "café", &"x".repeat(65),
        ] {
            assert!(!valid_command(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn sanitize_collapses_overstrike_bold_and_underline() {
        // Bold: each char doubled around a backspace.
        assert_eq!(sanitize("N\u{8}NA\u{8}AM\u{8}ME\u{8}E"), "NAME");
        // Underline: "_\bX".
        assert_eq!(sanitize("_\u{8}t_\u{8}e_\u{8}x_\u{8}t"), "text");
    }

    #[test]
    fn sanitize_strips_ansi_sgr() {
        assert_eq!(sanitize("\u{1b}[1mBold\u{1b}[0m text"), "Bold text");
        assert_eq!(sanitize("plain"), "plain");
    }

    #[test]
    fn sanitize_handles_mixed() {
        assert_eq!(sanitize("\u{1b}[4mg\u{8}gi\u{8}it\u{8}t\u{1b}[0m"), "git");
    }
}
