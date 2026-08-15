//! Parse `df(1)` output into per-filesystem usage for the disk-free gauge view.
//!
//! When the user has typed `df` and presses the enhance shortcut, the bridge runs a
//! read-only `df -k` and hands its output here. `parse_df` turns the table into one
//! [`FsUsage`] per mounted filesystem (size / used / available / use%), which the frontend
//! renders as proportional gauges. Purely informational — nothing is composed or run.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: input whose first line
//! isn't df's header (or that yields no valid rows) returns `None`, mirroring the
//! `ps-decorate` / `dumap` / `freemem` gate discipline.

use serde::{Deserialize, Serialize};

/// One filesystem row from `df -k` (sizes in KiB).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsUsage {
    pub filesystem: String,
    pub size_kb: u64,
    pub used_kb: u64,
    pub avail_kb: u64,
    /// The `Use%` column as a whole number (e.g. `71`).
    pub use_pct: u32,
    pub mount: String,
}

/// Parse `df -k` output into per-filesystem rows. The first non-empty line must be df's
/// header (`Filesystem … Mounted on`); each subsequent line is `fs size used avail use%
/// mount…` (the mount path, which may contain spaces, is the remainder). A row with too few
/// columns or non-numeric sizes is skipped. `None` if the header doesn't match or no row
/// parses.
pub fn parse_df(output: &str) -> Option<Vec<FsUsage>> {
    let mut lines = output.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    if !(header.contains("Filesystem") && header.contains("Mounted")) {
        return None;
    }
    let mut rows = Vec::new();
    for line in lines {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 6 {
            continue; // malformed / wrapped row — skip
        }
        let (Ok(size_kb), Ok(used_kb), Ok(avail_kb)) =
            (t[1].parse::<u64>(), t[2].parse::<u64>(), t[3].parse::<u64>())
        else {
            continue;
        };
        let Ok(use_pct) = t[4].trim_end_matches('%').parse::<u32>() else {
            continue; // e.g. "-" for some pseudo filesystems
        };
        rows.push(FsUsage {
            filesystem: t[0].to_string(),
            size_kb,
            used_kb,
            avail_kb,
            use_pct,
            mount: t[5..].join(" "),
        });
    }
    (!rows.is_empty()).then_some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Filesystem     1K-blocks      Used Available Use% Mounted on
tmpfs            3151004      4532   3146472   1% /run
/dev/nvme0n1p5 461742992 310815316 127398972  71% /
efivarfs             438       235       199  55% /sys/firmware/efi/efivars
";

    #[test]
    fn parses_filesystems() {
        let rows = parse_df(SAMPLE).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].filesystem, "/dev/nvme0n1p5");
        assert_eq!(rows[1].size_kb, 461742992);
        assert_eq!(rows[1].used_kb, 310815316);
        assert_eq!(rows[1].avail_kb, 127398972);
        assert_eq!(rows[1].use_pct, 71);
        assert_eq!(rows[1].mount, "/");
    }

    #[test]
    fn mount_path_with_spaces_is_preserved() {
        let out = "Filesystem 1K-blocks Used Available Use% Mounted on\n\
/dev/sdb1 1000 400 600 40% /mnt/My Backup Drive\n";
        let rows = parse_df(out).unwrap();
        assert_eq!(rows[0].mount, "/mnt/My Backup Drive");
    }

    #[test]
    fn skips_rows_with_dash_usepct() {
        let out = "Filesystem 1K-blocks Used Available Use% Mounted on\n\
good 100 40 60 40% /a\n\
weird 0 0 0 - /b\n";
        let rows = parse_df(out).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mount, "/a");
    }

    #[test]
    fn non_df_is_none() {
        assert!(parse_df("").is_none());
        assert!(parse_df("total 48\nfoo bar\n").is_none());
        // Header present but no data rows → None.
        assert!(parse_df("Filesystem 1K-blocks Used Available Use% Mounted on\n").is_none());
    }
}
