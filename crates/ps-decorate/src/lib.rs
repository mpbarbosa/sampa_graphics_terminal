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

use serde::{Deserialize, Serialize};

/// The effective enhancement level (spec §3). Mirrors `sampa-config`'s `PsEnhance`, but
/// kept here so this crate stays independent of `config` — the bridge maps between them.
/// Higher levels build on lower ones; [`resolve_level`] steps down on narrow terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    Quiet,
    Bars,
    Inspector,
}

/// Per-level minimum terminal widths (spec §3). Below `min_width` nothing is enhanced;
/// each richer level needs its own threshold or it falls back one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthThresholds {
    pub min_width: u16,
    pub min_width_bars: u16,
    pub min_width_inspector: u16,
}

/// Resolve the level actually rendered, after width fallback (spec §3): below `min_width`
/// nothing is enhanced; otherwise each level steps down one when the terminal is narrower
/// than that level's threshold (inspector → bars → quiet).
pub fn resolve_level(configured: Level, cols: u16, t: &WidthThresholds) -> Level {
    if cols < t.min_width {
        return Level::Off;
    }
    match configured {
        Level::Inspector if cols < t.min_width_inspector => resolve_level(Level::Bars, cols, t),
        Level::Bars if cols < t.min_width_bars => Level::Quiet,
        other => other,
    }
}

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuietRow {
    pub pid: String,
    pub user: String,
    /// `"–"` for an exact-zero measurement, else the original digits (spec §4).
    pub cpu: String,
    pub mem: String,
    /// Human size (`18.9M`) or `"–"` when resident memory is zero.
    pub rss: String,
    /// Raw resident size in KiB, so the frontend can sort memory by size (spec §5 sort).
    pub rss_kb: u64,
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

/// Width of a signal bar, in cells (spec §5).
pub const BAR_CELLS: usize = 8;

/// An 8-cell magnitude bar for `value` scaled against `max` (spec §5 "signal bars").
/// Drawn with block-element glyphs — the full block `█` (U+2588) and the left eighth-block
/// series (U+258F..U+2589) — so the bar **survives copy/paste as text**, padded with
/// spaces to a fixed [`BAR_CELLS`] width so columns align. `max <= 0` yields an empty bar
/// (nothing to compare against). Scaling is against the column max in the result set, never
/// a fixed 0–100, so an idle machine's small values still spread across the width.
pub fn bar(value: f32, max: f32) -> String {
    let frac = if max > 0.0 {
        (value / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Total fill measured in eighths-of-a-cell across the whole bar.
    let eighths_total = (frac * (BAR_CELLS * 8) as f32).round() as usize;
    let full = eighths_total / 8;
    let rem = eighths_total % 8;
    let mut s = String::with_capacity(BAR_CELLS);
    for _ in 0..full {
        s.push('\u{2588}'); // full block
    }
    let mut cells = full;
    if rem > 0 && full < BAR_CELLS {
        // U+2588 (8/8) down to U+258F (1/8): codepoint 0x2590 - eighths.
        s.push(char::from_u32(0x2590 - rem as u32).unwrap());
        cells += 1;
    }
    for _ in cells..BAR_CELLS {
        s.push(' ');
    }
    s
}

/// Per-row signal bars for the CPU and MEM columns (spec §5), each scaled against that
/// column's maximum across the shown rows. Returned parallel to `quiet.rows`.
pub fn bars_for(quiet: &Quiet) -> Vec<(String, String)> {
    let cpu_max = quiet.rows.iter().map(|r| r.cpu_val).fold(0.0_f32, f32::max);
    let mem_max = quiet.rows.iter().map(|r| r.mem_val).fold(0.0_f32, f32::max);
    quiet
        .rows
        .iter()
        .map(|r| (bar(r.cpu_val, cpu_max), bar(r.mem_val, mem_max)))
        .collect()
}

/// Provenance group for a process (spec §6 "grouping") — what it *belongs to*, so a flat
/// list of 300 becomes six things a developer recognises. Derived here from the executable
/// name / command; the frontend can refine with the process tree from the enrich query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Dev,
    Browser,
    Desktop,
    System,
    Shell,
    Other,
}

/// Fixed display order (spec §6): the things a developer most wants first.
pub const GROUP_ORDER: &[Group] = &[
    Group::Dev,
    Group::Browser,
    Group::Desktop,
    Group::System,
    Group::Shell,
    Group::Other,
];

/// Keyword tables, matched against the lowercased command. Order of the checks below is
/// deliberate: browser/dev/desktop win over the root→System fallback.
const BROWSER_KW: &[&str] = &[
    "firefox", "chrome", "chromium", "webkit", "epiphany", "brave", "vivaldi", "opera",
    "web content", "gecko", "thunderbird",
];
const DEV_KW: &[&str] = &[
    "node", "vite", "esbuild", "webpack", "python", "cargo", "rustc", "rust-analyzer",
    "gopls", "gradle", "postgres", "mysqld", "redis", "docker", "podman", "containerd",
    "clangd", "deno", "bun", "tsserver", "code", "ruby", "java",
];
const DESKTOP_KW: &[&str] = &[
    "gnome", "xwayland", "xorg", "wayland", "kwin", "plasma", "mutter", "sway", "gdm",
    "sddm", "pipewire", "pulseaudio", "wireplumber", "ibus", "fcitx", "kde", "xdg-",
];
const SHELL_EXE: &[&str] = &["bash", "zsh", "sh", "fish", "tmux", "screen", "ps", "dash"];

/// Whether `kw` occurs in `cmd` (already lowercased) at a **word boundary**, so a keyword
/// isn't matched inside an unrelated word: plain `contains` mis-tagged `--session=ubuntu`
/// as `bun` (Dev) and `nodev` as `node`. The match must start at a non-alphanumeric
/// boundary; trailing digits are allowed (so `python3` still matches `python`) but a
/// trailing letter is not (so `dockerd` doesn't match `docker`, `nodev` doesn't match
/// `node`). A keyword ending in a non-alphanumeric char (e.g. `xdg-`) is a prefix pattern,
/// so its right side isn't boundary-checked.
fn kw_at_boundary(cmd: &str, kw: &str) -> bool {
    let bytes = cmd.as_bytes();
    let prefix = !kw.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric);
    cmd.match_indices(kw).any(|(i, _)| {
        let left = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let right = prefix || {
            let mut j = i + kw.len();
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            j >= bytes.len() || !bytes[j].is_ascii_alphanumeric()
        };
        left && right
    })
}

/// Classify a process by its command and owning user (spec §6). Kernel threads are folded
/// out before this runs, so there is no `Kernel` case here.
pub fn classify(command: &str, user: &str) -> Group {
    let cmd = command.to_lowercase();
    let has = |kws: &[&str]| kws.iter().any(|k| kw_at_boundary(&cmd, k));
    // The executable basename (first token, after the last '/').
    let exe = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_lowercase();

    if has(BROWSER_KW) {
        Group::Browser
    } else if has(DEV_KW) {
        Group::Dev
    } else if has(DESKTOP_KW) {
        Group::Desktop
    } else if SHELL_EXE.contains(&exe.as_str()) {
        Group::Shell
    } else if user == "root" || cmd.contains("systemd") || cmd.contains("dbus") {
        Group::System
    } else {
        Group::Other
    }
}

/// A provenance group with its member rows and subtotals (spec §6). `rows` holds indices
/// into the `QuietRow` slice passed to [`group_rows`], in their original order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupView {
    pub group: Group,
    pub rows: Vec<usize>,
    pub count: usize,
    /// Summed %CPU across the group — "what is my machine actually doing" at a glance.
    pub cpu: f32,
    /// Summed resident memory (KiB) across the group.
    pub rss_kb: u64,
}

/// Bucket decorated rows into provenance groups with per-group CPU/RSS subtotals
/// (spec §6), returned in [`GROUP_ORDER`]; empty groups are omitted.
pub fn group_rows(rows: &[QuietRow]) -> Vec<GroupView> {
    GROUP_ORDER
        .iter()
        .filter_map(|&g| {
            let idx: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| classify(&r.command, &r.user) == g)
                .map(|(i, _)| i)
                .collect();
            if idx.is_empty() {
                return None;
            }
            let cpu = idx.iter().map(|&i| rows[i].cpu_val).sum();
            let rss_kb = idx.iter().map(|&i| rows[i].rss_kb).sum();
            Some(GroupView {
                group: g,
                count: idx.len(),
                cpu,
                rss_kb,
                rows: idx,
            })
        })
        .collect()
}

/// The `ps -o` field list the enrich query requests, so the bridge and [`parse_enrich`]
/// agree on the column order. Trailing `=` suppresses the header (one clean data line per
/// process): pid, parent pid, thread count, full state code, elapsed seconds.
pub const ENRICH_FORMAT: &str = "pid=,ppid=,nlwp=,stat=,etimes=";

/// Per-process detail the scraped `ps aux` table doesn't carry (spec §6 detail pane),
/// obtained by a supplementary read-only `ps` query keyed on PID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PsDetail {
    pub pid: u32,
    pub ppid: u32,
    pub threads: u32,
    /// Raw `ps` state code (e.g. `Ss`, `Sl`, `R+`).
    pub state: String,
    /// Human expansion of the state's first letter (e.g. "sleeping").
    pub state_long: String,
    /// Seconds since the process started (the frontend derives absolute start = now − this).
    pub etimes: u64,
}

/// Expand a `ps` STAT code's leading letter into a word (spec §6 "full state").
pub fn describe_state(stat: &str) -> String {
    match stat.chars().next() {
        Some('R') => "running",
        Some('S') => "sleeping",
        Some('D') => "uninterruptible sleep",
        Some('I') => "idle",
        Some('T') => "stopped",
        Some('t') => "traced",
        Some('Z') => "zombie",
        Some('X') => "dead",
        _ => "unknown",
    }
    .to_string()
}

/// Parse the output of `ps -o `[`ENRICH_FORMAT`]` -p …` into per-process details. Each line
/// is `pid ppid nlwp stat etimes`; a malformed line is skipped (enrich is best-effort — a
/// process may exit between the scrape and the query), never aborting the batch.
pub fn parse_enrich(output: &str) -> Vec<PsDetail> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() != 5 {
                return None;
            }
            Some(PsDetail {
                pid: f[0].parse().ok()?,
                ppid: f[1].parse().ok()?,
                threads: f[2].parse().ok()?,
                state: f[3].to_string(),
                state_long: describe_state(f[3]),
                etimes: f[4].parse().ok()?,
            })
        })
        .collect()
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

/// Build the 1a decorated model from parsed rows: drop `VSZ`, elide zeros, format sizes,
/// and fold bracketed kernel threads into a count.
fn build_quiet(rows: Vec<AuxRow>) -> Quiet {
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
            rss_kb: r.rss,
            start: r.start,
            command: r.command,
            cpu_val: r.cpu,
            mem_val: r.mem,
        })
        .collect();
    Quiet {
        rows: quiet_rows,
        kernel_count,
    }
}

/// Full level-1a decoration of a **clean** `ps aux` block (header first, every following
/// line a data row). Fails safe: `None` on any header mismatch or malformed row (spec §3).
/// Use this for a captured-stream block whose bounds are known exactly.
pub fn decorate_quiet(block: &str) -> Option<Quiet> {
    Some(build_quiet(parse_aux_block(block)?))
}

/// Decorate a `ps aux` table found **anywhere inside a scrollback scrape**, where the
/// exact bounds aren't known. Unlike [`decorate_quiet`], leading noise before the header
/// (prompt, the typed command) and trailing noise after the last row (the next prompt)
/// are tolerated: locate the first `aux` header line, then parse consecutive data rows and
/// **stop** at the first line that isn't one (a blank line or the returned prompt). This
/// is the entry point for the manual, buffer-scrape trigger. `None` if no `aux` header is
/// present or no rows follow it — the caller then leaves the raw output untouched.
pub fn decorate_scrollback(block: &str) -> Option<Quiet> {
    let lines: Vec<&str> = block.lines().collect();
    let header_at = lines
        .iter()
        .position(|l| header_kind(l) == Some(HeaderKind::Aux))?;
    let mut rows = Vec::new();
    for line in &lines[header_at + 1..] {
        match parse_aux_row(line) {
            Some(r) => rows.push(r),
            None => break, // end of the table (blank line, next prompt, …)
        }
    }
    if rows.is_empty() {
        return None;
    }
    Some(build_quiet(rows))
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

    const THRESH: WidthThresholds = WidthThresholds {
        min_width: 80,
        min_width_bars: 100,
        min_width_inspector: 120,
    };

    #[test]
    fn width_fallback() {
        // Below the floor, nothing is enhanced regardless of the configured level.
        assert_eq!(resolve_level(Level::Inspector, 79, &THRESH), Level::Off);
        assert_eq!(resolve_level(Level::Quiet, 79, &THRESH), Level::Off);
        // off stays off.
        assert_eq!(resolve_level(Level::Off, 200, &THRESH), Level::Off);
        // quiet holds from its floor up.
        assert_eq!(resolve_level(Level::Quiet, 80, &THRESH), Level::Quiet);
        // bars steps down to quiet below 100, holds at/above.
        assert_eq!(resolve_level(Level::Bars, 99, &THRESH), Level::Quiet);
        assert_eq!(resolve_level(Level::Bars, 100, &THRESH), Level::Bars);
        // inspector steps down to bars below 120, and all the way to quiet below 100.
        assert_eq!(resolve_level(Level::Inspector, 119, &THRESH), Level::Bars);
        assert_eq!(resolve_level(Level::Inspector, 99, &THRESH), Level::Quiet);
        assert_eq!(resolve_level(Level::Inspector, 120, &THRESH), Level::Inspector);
    }

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
    fn enrich_parse() {
        // As `ps -o pid=,ppid=,nlwp=,stat=,etimes= -p 1,3140` prints it.
        let out = "      1       0    1 Ss     128594\n   3140    2411   11 Sl      2480\n";
        let d = parse_enrich(out);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].pid, 1);
        assert_eq!(d[0].ppid, 0);
        assert_eq!(d[0].threads, 1);
        assert_eq!(d[0].state, "Ss");
        assert_eq!(d[0].state_long, "sleeping");
        assert_eq!(d[0].etimes, 128594);
        assert_eq!(d[1].pid, 3140);
        assert_eq!(d[1].threads, 11);
        assert_eq!(d[1].state_long, "sleeping");
        // Malformed / short lines are skipped, not fatal.
        assert!(parse_enrich("garbage\n\n42 only two\n").is_empty());
        assert_eq!(describe_state("R+"), "running");
        assert_eq!(describe_state("Zl"), "zombie");
    }

    #[test]
    fn provenance_classify() {
        assert_eq!(classify("/usr/lib/firefox/firefox", "mp"), Group::Browser);
        assert_eq!(classify("Web Content (guia)", "mp"), Group::Browser);
        assert_eq!(classify("node /home/mp/vite dev --host", "mp"), Group::Dev);
        assert_eq!(classify("postgres: checkpointer", "postgres"), Group::Dev);
        assert_eq!(classify("/usr/bin/gnome-shell", "gdm"), Group::Desktop);
        assert_eq!(classify("bash", "mp"), Group::Shell);
        assert_eq!(classify("-zsh", "mp"), Group::Other); // login shell dash-prefixed exe
        assert_eq!(classify("/usr/lib/systemd/systemd-journald", "root"), Group::System);
        assert_eq!(classify("/sbin/init splash", "root"), Group::System); // root fallback
        assert_eq!(classify("some-random-thing", "mp"), Group::Other);
        // Word-boundary matching: a keyword inside an unrelated word must NOT match.
        // "ubuntu" contains "bun", "nodev" contains "node" — both previously mis-tagged Dev.
        assert_eq!(
            classify("/usr/libexec/gdm-wayland-session /usr/bin/gnome-session --session=ubuntu", "mp"),
            Group::Desktop,
        );
        assert_eq!(classify("/usr/bin/gnome-shell --mode=ubuntu", "mp"), Group::Desktop);
        assert_eq!(classify("fusermount3 -o rw,nosuid,nodev,fsname=portal", "root"), Group::System);
        assert_eq!(classify("/home/mp/.local/bundles/current/jetbrainsd run", "mp"), Group::Other);
        // Trailing digits still match (pythonN is python); prefix keywords (xdg-) still match.
        assert_eq!(classify("/usr/bin/python3 /usr/bin/blueman-applet", "mp"), Group::Dev);
        assert_eq!(classify("/usr/libexec/xdg-desktop-portal", "mp"), Group::Desktop);
        // A trailing letter does not (dockerd ≠ docker), but the --containerd flag is a whole word.
        assert_eq!(classify("/usr/bin/dockerd -H fd:// --containerd=/run/c.sock", "root"), Group::Dev);
    }

    #[test]
    fn grouping_with_subtotals() {
        // A mixed block: browser (firefox), dev (node), shell (ps aux), system (init).
        let block = format!(
            "{AUX_HEADER_LINE}\n\
mp     100  8.0  5.0 100 800000 ? Sl ago11 1:00 /usr/lib/firefox/firefox\n\
mp     200 12.0  6.0 100 900000 ? Sl ago11 2:00 node vite dev\n\
mp     300  0.0  0.0 100   3000 pts/0 R+ ago11 0:00 ps aux\n\
root     1  0.0  0.0 100  19376 ? Ss ago11 0:11 /sbin/init splash\n"
        );
        let q = decorate_quiet(&block).unwrap();
        let groups = group_rows(&q.rows);
        // Order is GROUP_ORDER: Dev, Browser, Desktop, System, Shell, Other.
        let names: Vec<Group> = groups.iter().map(|g| g.group).collect();
        assert_eq!(names, vec![Group::Dev, Group::Browser, Group::System, Group::Shell]);
        let dev = &groups[0];
        assert_eq!(dev.count, 1);
        assert_eq!(dev.cpu, 12.0); // node
        assert_eq!(dev.rss_kb, 900000);
        // Every shown row is placed in exactly one group.
        let total: usize = groups.iter().map(|g| g.count).sum();
        assert_eq!(total, q.rows.len());
    }

    #[test]
    fn signal_bars() {
        // Fixed width, always BAR_CELLS cells (padded with spaces).
        assert_eq!(bar(0.0, 100.0).chars().count(), BAR_CELLS);
        assert_eq!(bar(50.0, 100.0).chars().count(), BAR_CELLS);
        // Empty when there's nothing to scale against.
        assert_eq!(bar(5.0, 0.0), "        ");
        assert_eq!(bar(0.0, 100.0), "        ");
        // Full bar is all full blocks.
        assert_eq!(bar(100.0, 100.0), "████████");
        // Half → four full blocks then spaces.
        assert_eq!(bar(50.0, 100.0), "████    ");
        // A small fraction shows a partial left-eighth glyph in the first cell.
        assert_eq!(bar(1.0, 100.0), "\u{258F}       ");
        // Clamps above max.
        assert_eq!(bar(150.0, 100.0), "████████");
    }

    #[test]
    fn bars_scale_to_column_max() {
        let q = decorate_quiet(BLOCK).unwrap();
        let bars = bars_for(&q);
        assert_eq!(bars.len(), q.rows.len());
        // init is 0.0 cpu → empty bar; node is the column max (12.4) → full bar.
        assert_eq!(bars[0].0, "        ");
        assert_eq!(bars[1].0, "████████");
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
    fn scrollback_tolerates_surrounding_noise() {
        // As scraped from the terminal: a prompt + the typed command before the header,
        // and the returned prompt after the last row.
        let scraped = format!(
            "mp@host ~/proj % ps aux\n{BLOCK}mp@host ~/proj % "
        );
        let q = decorate_scrollback(&scraped).unwrap();
        assert_eq!(q.kernel_count, 2);
        assert_eq!(q.rows.len(), 2);
        assert_eq!(q.rows[0].pid, "1");
        assert_eq!(q.rows[1].rss, "961.2M");

        // No ps header anywhere → nothing to decorate.
        assert!(decorate_scrollback("just some\nrandom output\n").is_none());
        // Header but no rows follow → None.
        assert!(decorate_scrollback(&format!("{AUX_HEADER_LINE}\nmp@host % ")).is_none());
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
