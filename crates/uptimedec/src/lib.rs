//! Parse `uptime(1)` output into load averages + uptime/users for the load gauge view.
//!
//! When the user has typed `uptime` and presses the enhance shortcut, the bridge runs
//! `uptime` **with `LC_ALL=C`** and hands its output here. Forcing the C locale matters:
//! under some locales `uptime` prints load averages with a decimal **comma**
//! (`0,74, 0,30, 0,17`), where the comma is both the decimal point *and* the list separator
//! — unparseable. In C it is `0.74, 0.30, 0.17`, which this parses into the 1/5/15-minute
//! averages plus the best-effort uptime-duration and user-count for display.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: a line without
//! `load average` yields `None`, mirroring the `ps-decorate` / `dumap` / `freemem` gate.

use serde::{Deserialize, Serialize};

/// The three load averages plus the uptime-duration and user-count text (best-effort).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UptimeInfo {
    /// The uptime duration text, e.g. `1 day,  3:11` (as `uptime` printed it), if found.
    pub up: Option<String>,
    /// Logged-in user count, if found.
    pub users: Option<u32>,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

/// Parse `uptime` output (C locale). `None` if no `load average` line or fewer than three
/// numeric load values.
pub fn parse_uptime(output: &str) -> Option<UptimeInfo> {
    let line = output.lines().find(|l| l.contains("load average"))?;
    let after = line.split("load average:").nth(1)?;
    let loads: Vec<f64> = after
        .split(',')
        .filter_map(|x| x.trim().parse::<f64>().ok())
        .collect();
    if loads.len() < 3 {
        return None;
    }
    Some(UptimeInfo {
        up: extract_up(line),
        users: extract_users(line),
        load1: loads[0],
        load5: loads[1],
        load15: loads[2],
    })
}

/// The uptime-duration text: between ` up ` and the ` user(s)` clause, minus the trailing
/// `, <count>` user number. Best-effort — `None` if the markers aren't found.
fn extract_up(line: &str) -> Option<String> {
    let start = line.find(" up ")? + 4;
    let user_idx = line.find(" user")?;
    if user_idx <= start {
        return None;
    }
    let seg = &line[start..user_idx];
    let up = match seg.rfind(',') {
        Some(i) => &seg[..i], // drop the ", <count>" that precedes "user"
        None => seg,
    };
    Some(up.trim().to_string())
}

/// The logged-in user count: the last integer before ` user`.
fn extract_users(line: &str) -> Option<u32> {
    let idx = line.find(" user")?;
    line[..idx]
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_c_locale_uptime() {
        let out = " 13:22:01 up 1 day,  3:11,  1 user,  load average: 0.74, 0.30, 0.17\n";
        let u = parse_uptime(out).unwrap();
        assert_eq!(u.load1, 0.74);
        assert_eq!(u.load5, 0.30);
        assert_eq!(u.load15, 0.17);
        assert_eq!(u.users, Some(1));
        assert_eq!(u.up.as_deref(), Some("1 day,  3:11"));
    }

    #[test]
    fn short_uptime_and_plural_users() {
        let out = " 09:05:00 up 12 min,  3 users,  load average: 2.50, 1.20, 0.90\n";
        let u = parse_uptime(out).unwrap();
        assert_eq!(u.load1, 2.50);
        assert_eq!(u.users, Some(3));
        assert_eq!(u.up.as_deref(), Some("12 min"));
    }

    #[test]
    fn no_load_line_is_none() {
        assert!(parse_uptime("").is_none());
        assert!(parse_uptime("some other output\n").is_none());
    }
}
