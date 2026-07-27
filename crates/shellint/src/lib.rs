//! Shell-integration scanner (DESIGN.md §5.6, §10.2).
//!
//! Terminals get *robust* command/cwd tracking when the shell cooperates by emitting
//! two families of OSC escape sequences:
//!
//! - **OSC 7** — `ESC ] 7 ; file://host/path ST` — the current working directory.
//! - **OSC 133** — semantic prompt marks: `A` (prompt start), `B` (command input
//!   start), `C` (command output start), `D[;code]` (command finished).
//!
//! [`OscScanner`] consumes the raw PTY byte stream incrementally (sequences may be
//! split across read chunks) and yields the [`ShellEvent`]s it recognises. It does
//! not modify or consume the bytes — the caller still forwards them to the renderer.
//! Unknown OSCs and all other escapes are ignored. Headless and std-only.

/// A recognised shell-integration signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// OSC 7: working directory (absolute path, percent-decoded).
    Cwd(String),
    /// OSC 133;A — a new prompt is about to be drawn.
    PromptStart,
    /// OSC 133;B — end of prompt; the user's command input begins here.
    CommandStart,
    /// OSC 133;C — the command is running; its output begins here.
    OutputStart,
    /// OSC 133;D[;code] — the command finished, with its exit code if provided.
    CommandEnd(Option<i32>),
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Ground,
    Esc,
    Osc,
    OscEsc,
}

/// Cap on a single OSC payload; anything longer is treated as junk and dropped.
const MAX_OSC: usize = 4096;

/// Incremental scanner. Create one per session and [`feed`](Self::feed) it each chunk.
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            overflow: false,
        }
    }

    /// Feed a chunk of PTY output; returns any shell events it completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ShellEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    if b == b']' {
                        self.state = State::Osc;
                        self.buf.clear();
                        self.overflow = false;
                    } else if b == 0x1b {
                        // stay in Esc
                    } else {
                        self.state = State::Ground;
                    }
                }
                State::Osc => match b {
                    0x07 => {
                        self.finish(&mut out);
                    }
                    0x1b => {
                        self.state = State::OscEsc;
                    }
                    _ => self.push(b),
                },
                State::OscEsc => {
                    if b == b'\\' {
                        self.finish(&mut out);
                    } else if b == 0x1b {
                        // another ESC; keep the first one as data, stay looking for ST
                        self.push(0x1b);
                    } else {
                        // lone ESC inside the OSC; keep both bytes as data
                        self.push(0x1b);
                        self.push(b);
                        self.state = State::Osc;
                    }
                }
            }
        }
        out
    }

    fn push(&mut self, b: u8) {
        if self.buf.len() < MAX_OSC {
            self.buf.push(b);
        } else {
            self.overflow = true;
        }
    }

    fn finish(&mut self, out: &mut Vec<ShellEvent>) {
        if !self.overflow {
            if let Some(ev) = parse_osc(&self.buf) {
                out.push(ev);
            }
        }
        self.buf.clear();
        self.overflow = false;
        self.state = State::Ground;
    }
}

/// Parse a completed OSC payload (the bytes between `ESC ]` and the terminator).
fn parse_osc(buf: &[u8]) -> Option<ShellEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    let (kind, rest) = s.split_once(';').unwrap_or((s, ""));
    match kind {
        "7" => parse_osc7(rest).map(ShellEvent::Cwd),
        "133" => {
            let mut it = rest.split(';');
            match it.next()? {
                "A" => Some(ShellEvent::PromptStart),
                "B" => Some(ShellEvent::CommandStart),
                "C" => Some(ShellEvent::OutputStart),
                "D" => Some(ShellEvent::CommandEnd(it.next().and_then(|c| c.parse().ok()))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `file://host/path` (or `file:///path`, or a bare `/path`) → the decoded path.
fn parse_osc7(uri: &str) -> Option<String> {
    let path = if let Some(rest) = uri.strip_prefix("file://") {
        let slash = rest.find('/')?; // path begins at the first '/' after the host
        &rest[slash..]
    } else if uri.starts_with('/') {
        uri // tolerate a bare path
    } else {
        return None;
    };
    Some(percent_decode(path))
}

/// Minimal `%XX` percent-decoding (cwd paths encode spaces etc.).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<ShellEvent> {
        let mut s = OscScanner::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.feed(c));
        }
        out
    }

    #[test]
    fn osc7_bel_and_st() {
        assert_eq!(
            scan(&[b"\x1b]7;file://host/tmp/foo\x07"]),
            vec![ShellEvent::Cwd("/tmp/foo".into())]
        );
        assert_eq!(
            scan(&[b"\x1b]7;file:///home/mpb\x1b\\"]),
            vec![ShellEvent::Cwd("/home/mpb".into())]
        );
    }

    #[test]
    fn osc7_percent_decoded() {
        assert_eq!(
            scan(&[b"\x1b]7;file://h/tmp/a%20b\x07"]),
            vec![ShellEvent::Cwd("/tmp/a b".into())]
        );
    }

    #[test]
    fn osc133_marks() {
        assert_eq!(scan(&[b"\x1b]133;A\x1b\\"]), vec![ShellEvent::PromptStart]);
        assert_eq!(scan(&[b"\x1b]133;B\x07"]), vec![ShellEvent::CommandStart]);
        assert_eq!(scan(&[b"\x1b]133;C\x07"]), vec![ShellEvent::OutputStart]);
        assert_eq!(scan(&[b"\x1b]133;D;7\x07"]), vec![ShellEvent::CommandEnd(Some(7))]);
        assert_eq!(scan(&[b"\x1b]133;D\x07"]), vec![ShellEvent::CommandEnd(None)]);
    }

    #[test]
    fn split_across_chunks() {
        assert_eq!(
            scan(&[b"\x1b]7;file://h/t", b"mp\x07"]),
            vec![ShellEvent::Cwd("/tmp".into())]
        );
        // Terminator split too.
        assert_eq!(scan(&[b"\x1b]133;A\x1b", b"\\"]), vec![ShellEvent::PromptStart]);
    }

    #[test]
    fn interleaved_with_normal_output_and_other_escapes() {
        let evs = scan(&[b"hello \x1b[31mred\x1b[0m\x1b]133;A\x1b\\ world\x1b]133;B\x07done"]);
        assert_eq!(evs, vec![ShellEvent::PromptStart, ShellEvent::CommandStart]);
    }

    #[test]
    fn full_command_cycle() {
        // precmd (D + cwd + A), prompt (B), preexec (C).
        let evs = scan(&[
            b"\x1b]133;D;0\x07\x1b]7;file://h/home\x07\x1b]133;A\x07",
            b"user@host $ \x1b]133;B\x07",
            b"ls\r\n\x1b]133;C\x07",
        ]);
        assert_eq!(
            evs,
            vec![
                ShellEvent::CommandEnd(Some(0)),
                ShellEvent::Cwd("/home".into()),
                ShellEvent::PromptStart,
                ShellEvent::CommandStart,
                ShellEvent::OutputStart,
            ]
        );
    }

    #[test]
    fn junk_and_unknown_osc_ignored() {
        assert_eq!(scan(&[b"\x1b]0;window title\x07"]), vec![]); // OSC 0 (title)
        assert_eq!(scan(&[b"\x1b]52;c;abc\x07"]), vec![]); // OSC 52 (clipboard)
        assert_eq!(scan(&[b"plain text no escapes"]), vec![]);
    }
}
