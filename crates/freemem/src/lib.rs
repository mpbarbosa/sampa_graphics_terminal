//! Parse `free(1)` output into memory/swap stats for the gauge view.
//!
//! When the user has typed `free` and presses the enhance shortcut, the bridge runs a
//! read-only `free -k` and hands its output here. `parse_free` turns the two-row table
//! into structured [`FreeInfo`] the frontend renders as proportional gauges (used /
//! buff-cache / free, plus swap). Purely informational — nothing is run.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: input without a usable
//! `Mem:` row yields `None`, mirroring the `ps-decorate` / `dumap` gate discipline.

use serde::{Deserialize, Serialize};

/// RAM stats (KiB), matching `free -k`'s `Mem:` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemStats {
    pub total_kb: u64,
    pub used_kb: u64,
    pub free_kb: u64,
    pub shared_kb: u64,
    pub buff_cache_kb: u64,
    /// Memory available for new work without swapping (kernel estimate).
    pub available_kb: u64,
}

/// Swap stats (KiB), from the `Swap:` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapStats {
    pub total_kb: u64,
    pub used_kb: u64,
    pub free_kb: u64,
}

/// Parsed `free` output: RAM always, swap when the row is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeInfo {
    pub mem: MemStats,
    pub swap: Option<SwapStats>,
}

/// Parse `free -k` output. Finds the `Mem:` row (required) and the `Swap:` row (optional),
/// reading the numeric columns after each label. `None` if there is no usable `Mem:` row
/// (fewer than the three core numbers) — the run-fresh caller controls the format, so this
/// is defensive.
pub fn parse_free(output: &str) -> Option<FreeInfo> {
    let mut mem: Option<MemStats> = None;
    let mut swap: Option<SwapStats> = None;
    for line in output.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.first() {
            Some(&"Mem:") => {
                let n: Vec<u64> = t[1..].iter().filter_map(|x| x.parse().ok()).collect();
                if n.len() < 3 {
                    return None; // need at least total/used/free
                }
                let buff_cache = n.get(4).copied().unwrap_or(0);
                mem = Some(MemStats {
                    total_kb: n[0],
                    used_kb: n[1],
                    free_kb: n[2],
                    shared_kb: n.get(3).copied().unwrap_or(0),
                    buff_cache_kb: buff_cache,
                    // Older `free` omits the "available" column; fall back to free+cache.
                    available_kb: n.get(5).copied().unwrap_or(n[2] + buff_cache),
                });
            }
            Some(&"Swap:") => {
                let n: Vec<u64> = t[1..].iter().filter_map(|x| x.parse().ok()).collect();
                if n.len() >= 3 {
                    swap = Some(SwapStats { total_kb: n[0], used_kb: n[1], free_kb: n[2] });
                }
            }
            _ => {}
        }
    }
    mem.map(|m| FreeInfo { mem: m, swap })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "               total        used        free      shared  buff/cache   available
Mem:        31510036     8176228     2753396     1373364    22519056    23333808
Swap:        2097148           0     2097148
";

    #[test]
    fn parses_mem_and_swap() {
        let f = parse_free(SAMPLE).unwrap();
        assert_eq!(f.mem.total_kb, 31510036);
        assert_eq!(f.mem.used_kb, 8176228);
        assert_eq!(f.mem.free_kb, 2753396);
        assert_eq!(f.mem.shared_kb, 1373364);
        assert_eq!(f.mem.buff_cache_kb, 22519056);
        assert_eq!(f.mem.available_kb, 23333808);
        let s = f.swap.unwrap();
        assert_eq!(s.total_kb, 2097148);
        assert_eq!(s.used_kb, 0);
        assert_eq!(s.free_kb, 2097148);
    }

    #[test]
    fn swap_optional() {
        let out = "               total        used        free\nMem:  1000  400  600\n";
        let f = parse_free(out).unwrap();
        assert_eq!(f.mem.total_kb, 1000);
        assert!(f.swap.is_none());
        // available falls back to free + buff_cache (0 here) when the column is absent.
        assert_eq!(f.mem.available_kb, 600);
    }

    #[test]
    fn no_mem_row_is_none() {
        assert!(parse_free("").is_none());
        assert!(parse_free("total used free\nSwap: 1 2 3\n").is_none());
    }
}
