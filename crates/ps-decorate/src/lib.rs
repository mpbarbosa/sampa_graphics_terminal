//! `ps(1)` output enhancement — headless parse-and-decorate core
//! (docs/spec-ps-output-enhancement.md).
//!
//! A TTY-only presentation layer over **unmodified** `ps` output. The emulator
//! recognises a `ps` table by its header signature, parses the rows, and hands the
//! frontend a decorated model. This crate is the durable, GUI-free core — the parser
//! and the "Quiet columns" (spec §4, level 1a) transforms. It has **no Tauri/webview
//! imports** (same rule as `pty-core`/`config`/`preview`) and is pure/deterministic so
//! it can be unit-tested against captured `ps` fixtures.
//!
//! ## Fail-safe to raw
//!
//! Every entry point returns `Option`/`None` rather than a partial result. Per spec §3:
//! an unrecognised header or a single malformed row **aborts enhancement for the whole
//! block** — the caller reprints the raw bytes. Partial parsing is never displayed, so a
//! stream that merely resembles `ps` can never be mangled.
//!
//! ## Scope of this slice
//!
//! Level 1a only: the deterministic transforms — zero elision, size units, kernel fold,
//! and the reduced (VSZ-dropped) column set. `START` locale-normalisation (§4) needs a
//! clock and a locale and is deferred; the raw `START` field is carried through so a
//! later pass can rewrite it. Levels 1b/1c (bars, inspector) build on this model.

/// Which `ps` header we matched. Only [`HeaderKind::Aux`] is decorated in this slice;
/// [`HeaderKind::Ef`] is recognised (so callers can gate) but passes through for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    /// `USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND`
    Aux,
    /// `UID PID PPID C STIME TTY TIME CMD`
    Ef,
}

/// Exact `ps aux` header field order (spec §3 — the match must be exact).
const AUX_HEADER: &[&str] = &[
    "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME", "COMMAND",
];

/// Exact `ps -ef` header field order.
const EF_HEADER: &[&str] = &["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"];

/// The reduced column set the 1a view presents (spec §4 — `VSZ` is dropped; `%CPU`/`%MEM`
/// keep the `ps` labels for familiarity but render elided/coloured downstream).
pub const QUIET_COLUMNS: &[&str] = &["PID", "USER", "%CPU", "%MEM", "RSS", "START", "COMMAND"];

/// One parsed `ps aux` row, fields still as strings/numbers exactly as `ps` printed them.
#[derive(Debug, Clone, PartialEq)]
pub struct AuxRow {
    pub user: String,
    pub pid: u32,
    pub cpu: f32,
    pub mem: f32,
    pub vsz: u64,
    pub rss: u64,
    pub tty: String,
    pub stat: String,
    pub start: String,
    pub time: String,
    pub command: String,
    /// `command` is a bracketed kernel thread, e.g. `[kthreadd]` (spec §4 kernel fold).
    pub is_kernel: bool,
}

/// A row after 1a decoration — display-ready cell strings for [`QUIET_COLUMNS`].
#[derive(Debug, Clone, PartialEq)]
pub struct QuietRow {
    pub pid: String,
    pub user: String,
    /// `"–"` for an exact-zero measurement, else the original digits (spec §4).
    pub cpu: String,
    pub mem: String,
    /// Human size (`18.9M`) or `"–"` when resident memory is zero.
    pub rss: String,
    pub start: String,
    pub command: String,
    /// Carried so the frontend can pick the colour band (spec §7) without re-parsing.
    pub cpu_val: f32,
    pub mem_val: f32,
}

/// The 1a decorated table: user-facing rows with kernel threads folded away.
#[derive(Debug, Clone, PartialEq)]
pub struct Quiet {
    pub rows: Vec<QuietRow>,
    /// How many bracketed kernel threads were folded out of `rows` (spec §4).
    pub kernel_count: usize,
}

impl Quiet {
    /// The single summary line that replaces the folded kernel rows (spec §4), or `None`
    /// when there was nothing to fold.
    pub fn kernel_summary(&self) -> Option<String> {
        (self.kernel_count > 0).then(|| {
            format!(
                "… {} kernel threads hidden (0.0% cpu, 0.0% mem) — ps aux --kernel to show",
                self.kernel_count
            )
        })
    }
}

/// Match the first line of a block against the known `ps` header signatures (spec §3).
/// Whitespace-insensitive but **order-exact**; anything else is `None` (raw passthrough).
pub fn header_kind(first_line: &str) -> Option<HeaderKind> {
    let fields: Vec<&str> = first_line.split_whitespace().collect();
    if fields == AUX_HEADER {
        Some(HeaderKind::Aux)
    } else if fields == EF_HEADER {
        Some(HeaderKind::Ef)
    } else {
        None
    }
}

/// Whether a command string is a bracketed kernel thread, e.g. `[kworker/0:0H]`.
pub fn is_kernel_command(command: &str) -> bool {
    let c = command.trim();
    c.len() >= 2 && c.starts_with('[') && c.ends_with(']')
}

/// Format a `ps` `RSS`/`VSZ` value (kibibytes) as K/M/G with one decimal, right-sized
/// (spec §4 "size units"). Removes the mental kB→MB division from every comparison.
pub fn fmt_size_kb(kb: u64) -> String {
    const MIB: f64 = 1024.0;
    const GIB: f64 = 1024.0 * 1024.0;
    let k = kb as f64;
    if k >= GIB {
        format!("{:.1}G", k / GIB)
    } else if k >= MIB {
        format!("{:.1}M", k / MIB)
    } else {
        format!("{}K", kb)
    }
}

/// Elide an exact-zero measurement to a dim rule (spec §4 "zero elision"): exact zero is
/// the *absence* of a measurement, not a measurement. Non-zero keeps one decimal.
fn elide_percent(v: f32) -> String {
    if v == 0.0 {
        "–".to_string()
    } else {
        format!("{:.1}", v)
    }
}

/// Take the first `n` whitespace-delimited fields and return them plus the untouched
/// remainder of the line (so `COMMAND`, which may contain spaces, is preserved verbatim).
/// `None` if the line runs out of fields before `n`.
fn split_fields(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(n);
    let mut rest = line;
    for _ in 0..n {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((fields, rest.trim()))
}

/// Parse a single `ps aux` data line into an [`AuxRow`]. `None` on any shape/number error
/// — the caller turns that into whole-block passthrough (spec §3).
pub fn parse_aux_row(line: &str) -> Option<AuxRow> {
    // 10 fixed fields, then COMMAND = the rest of the line (may contain spaces).
    let (f, command) = split_fields(line, 10)?;
    if command.is_empty() {
        return None;
    }
    Some(AuxRow {
        user: f[0].to_string(),
        pid: f[1].parse().ok()?,
        cpu: f[2].parse().ok()?,
        mem: f[3].parse().ok()?,
        vsz: f[4].parse().ok()?,
        rss: f[5].parse().ok()?,
        tty: f[6].to_string(),
        stat: f[7].to_string(),
        start: f[8].to_string(),
        time: f[9].to_string(),
        is_kernel: is_kernel_command(command),
        command: command.to_string(),
    })
}

/// Parse a whole `ps aux` block (header + data lines) into rows, or `None` if the header
/// is not `aux` or **any** data row is malformed (spec §3 fail-safe). Blank lines are
/// skipped; the header line is required to be first.
pub fn parse_aux_block(block: &str) -> Option<Vec<AuxRow>> {
    let mut lines = block.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    if header_kind(header) != Some(HeaderKind::Aux) {
        return None;
    }
    lines.map(parse_aux_row).collect()
}

/// Full level-1a decoration of a `ps aux` block: parse, drop `VSZ`, elide zeros, format
/// sizes, and fold bracketed kernel threads into a count. `None` ⇒ caller shows raw bytes.
pub fn decorate_quiet(block: &str) -> Option<Quiet> {
    let rows = parse_aux_block(block)?;
    let kernel_count = rows.iter().filter(|r| r.is_kernel).count();
    let quiet_rows = rows
        .into_iter()
        .filter(|r| !r.is_kernel)
        .map(|r| QuietRow {
            pid: r.pid.to_string(),
            user: r.user,
            cpu: elide_percent(r.cpu),
            mem: elide_percent(r.mem),
            rss: if r.rss == 0 {
                "–".to_string()
            } else {
                fmt_size_kb(r.rss)
            },
            start: r.start,
            command: r.command,
            cpu_val: r.cpu,
            mem_val: r.mem,
        })
        .collect();
    Some(Quiet {
        rows: quiet_rows,
        kernel_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUX_HEADER_LINE: &str =
        "USER   PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND";

    // A small, realistic block: init, one live dev process, two kernel threads.
    const BLOCK: &str = "USER   PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND\n\
root     1  0.0  0.0  29164 19376 ?        Ss   ago11    0:11 /sbin/init splash\n\
mp    3140 12.4  6.1 2100000 984320 ?      Sl   ago11   41:18 node /home/mp/vite dev --host\n\
root     2  0.0  0.0      0     0 ?        S    ago11    0:00 [kthreadd]\n\
root    13  0.0  0.0      0     0 ?        S    ago11    0:01 [ksoftirqd/0]\n";

    #[test]
    fn header_signatures() {
        assert_eq!(header_kind(AUX_HEADER_LINE), Some(HeaderKind::Aux));
        assert_eq!(
            header_kind("UID PID PPID C STIME TTY TIME CMD"),
            Some(HeaderKind::Ef)
        );
        // Wrong order / unknown header → passthrough.
        assert_eq!(header_kind("PID USER %CPU COMMAND"), None);
        assert_eq!(header_kind("total 48"), None);
        assert_eq!(header_kind(""), None);
    }

    #[test]
    fn kernel_detection() {
        assert!(is_kernel_command("[kthreadd]"));
        assert!(is_kernel_command("[kworker/0:0H-kblockd]"));
        assert!(!is_kernel_command("/sbin/init splash"));
        assert!(!is_kernel_command("node [not a kernel] thing")); // doesn't start with '['
        assert!(!is_kernel_command("[")); // single char, not a bracketed pair
    }

    #[test]
    fn size_units() {
        assert_eq!(fmt_size_kb(0), "0K");
        assert_eq!(fmt_size_kb(512), "512K");
        assert_eq!(fmt_size_kb(19376), "18.9M");
        assert_eq!(fmt_size_kb(984320), "961.2M");
        assert_eq!(fmt_size_kb(2 * 1024 * 1024), "2.0G");
    }

    #[test]
    fn parses_command_with_spaces() {
        let r = parse_aux_row(
            "mp    3140 12.4  6.1 2100000 984320 ?      Sl   ago11   41:18 node /home/mp/vite dev --host",
        )
        .unwrap();
        assert_eq!(r.pid, 3140);
        assert_eq!(r.cpu, 12.4);
        assert_eq!(r.rss, 984320);
        assert_eq!(r.command, "node /home/mp/vite dev --host");
        assert!(!r.is_kernel);
    }

    #[test]
    fn malformed_row_is_none() {
        // Non-numeric PID.
        assert!(parse_aux_row("root  xx  0.0 0.0 0 0 ? S ago11 0:00 foo").is_none());
        // Too few fields (no COMMAND).
        assert!(parse_aux_row("root 1 0.0 0.0 0 0 ? S ago11 0:11").is_none());
    }

    #[test]
    fn malformed_block_fails_safe() {
        let bad = format!("{AUX_HEADER_LINE}\nroot 1 0.0 0.0 0 0 ? S ago11 0:11 ok\ngarbage line\n");
        assert!(decorate_quiet(&bad).is_none());
        // Non-ps text never matches.
        assert!(decorate_quiet("hello\nworld\n").is_none());
    }

    #[test]
    fn quiet_decoration_end_to_end() {
        let q = decorate_quiet(BLOCK).unwrap();
        // Two kernel threads folded; two real rows remain.
        assert_eq!(q.kernel_count, 2);
        assert_eq!(q.rows.len(), 2);

        let init = &q.rows[0];
        assert_eq!(init.pid, "1");
        assert_eq!(init.cpu, "–"); // exact zero elided
        assert_eq!(init.mem, "–");
        assert_eq!(init.rss, "18.9M"); // 19376 kB → units
        assert_eq!(init.command, "/sbin/init splash");

        let node = &q.rows[1];
        assert_eq!(node.cpu, "12.4"); // non-zero kept
        assert_eq!(node.rss, "961.2M");
        assert_eq!(node.cpu_val, 12.4); // colour-band value carried through

        assert_eq!(
            q.kernel_summary().as_deref(),
            Some("… 2 kernel threads hidden (0.0% cpu, 0.0% mem) — ps aux --kernel to show")
        );
    }

    #[test]
    fn no_kernel_no_summary() {
        let block = format!("{AUX_HEADER_LINE}\nmp 100 1.0 1.0 100 2048 ? S ago11 0:01 bash\n");
        let q = decorate_quiet(&block).unwrap();
        assert_eq!(q.kernel_count, 0);
        assert!(q.kernel_summary().is_none());
        assert_eq!(q.rows[0].rss, "2.0M");
    }
}
