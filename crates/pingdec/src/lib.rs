//! Parse `ping(8)` output into a latency series + summary for the ping chart view.
//!
//! When the user has typed `ping <host>` and presses the enhance shortcut, the bridge runs
//! a **bounded** `ping` (fixed count) and hands its output here. `parse_ping` extracts each
//! reply's `icmp_seq` + `time`, plus the transmitted/received/loss and rtt min/avg/max/mdev
//! summary, so the frontend can draw a per-packet latency chart. Purely a diagnostic view;
//! nothing is composed or run beyond the ping the user asked for.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no replies
//! and no stats line yields `None`, mirroring the `ps-decorate` / `dumap` / `freemem` gate.

use serde::{Deserialize, Serialize};

/// One echo reply: its sequence number and round-trip time in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub seq: u32,
    pub time_ms: f64,
}

/// The rtt summary line (`rtt min/avg/max/mdev = …`), milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rtt {
    pub min: f64,
    pub avg: f64,
    pub max: f64,
    pub mdev: f64,
}

/// A parsed `ping` run: target, per-reply latencies, and the tail summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PingReport {
    /// The host as `ping` echoed it (from the `PING <host> (<ip>)` header), if present.
    pub host: Option<String>,
    /// The resolved address (between the header's parentheses), if present.
    pub ip: Option<String>,
    pub replies: Vec<Reply>,
    pub transmitted: u32,
    pub received: u32,
    /// Packet-loss percentage from the stats line (e.g. `0`, `10`, `33.3`).
    pub loss_pct: f64,
    /// The rtt summary, when `ping` printed it (absent if all packets were lost).
    pub rtt: Option<Rtt>,
}

/// Substring of `s` immediately following the first occurrence of `pat`, or `None`.
fn after<'a>(s: &'a str, pat: &str) -> Option<&'a str> {
    s.find(pat).map(|i| &s[i + pat.len()..])
}

/// Leading numeric run (digits and one dot) of `s`, parsed as f64.
fn lead_f64(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let n: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    n.parse().ok()
}
fn lead_u32(s: &str) -> Option<u32> {
    let t = s.trim_start();
    let n: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok()
}

/// Parse an echo-reply line: needs both `icmp_seq=` and `time=`.
fn parse_reply(line: &str) -> Option<Reply> {
    let seq = lead_u32(after(line, "icmp_seq=")?)?;
    let time_ms = lead_f64(after(line, "time=")?)?;
    Some(Reply { seq, time_ms })
}

/// Parse `ping` output. `None` if it has neither replies nor a stats line (not ping output).
pub fn parse_ping(output: &str) -> Option<PingReport> {
    let mut host = None;
    let mut ip = None;
    let mut replies = Vec::new();
    let (mut transmitted, mut received, mut loss_pct) = (0u32, 0u32, 0.0f64);
    let mut rtt = None;
    let mut saw_stats = false;

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("PING ") {
            // `PING host (1.2.3.4) 56(84) bytes of data.`
            let mut it = rest.split_whitespace();
            host = it.next().map(|s| s.to_string());
            if let (Some(a), Some(b)) = (rest.find('('), rest.find(')')) {
                if a < b {
                    ip = Some(rest[a + 1..b].to_string());
                }
            }
        } else if line.contains("icmp_seq=") && line.contains("time=") {
            if let Some(r) = parse_reply(line) {
                replies.push(r);
            }
        } else if line.contains("packets transmitted") {
            // `29 packets transmitted, 29 received, 0% packet loss, time 28157ms`
            saw_stats = true;
            let mut parts = line.split(',');
            if let Some(p) = parts.next() {
                transmitted = lead_u32(p).unwrap_or(0);
            }
            if let Some(p) = parts.next() {
                received = lead_u32(p).unwrap_or(0);
            }
            if let Some(p) = parts.next() {
                loss_pct = lead_f64(p).unwrap_or(0.0);
            }
        } else if let Some(rest) = after(line, "min/avg/max") {
            // `rtt min/avg/max/mdev = 6.119/13.832/29.224/5.442 ms`
            if let Some(nums) = after(rest, "= ").or_else(|| after(rest, "=")) {
                let vals: Vec<f64> = nums
                    .split('/')
                    .filter_map(|x| lead_f64(x))
                    .collect();
                if vals.len() >= 4 {
                    rtt = Some(Rtt { min: vals[0], avg: vals[1], max: vals[2], mdev: vals[3] });
                }
            }
        }
    }

    if replies.is_empty() && !saw_stats {
        return None;
    }
    Some(PingReport { host, ip, replies, transmitted, received, loss_pct, rtt })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from the reference screenshot.
    const SAMPLE: &str = "PING dftex7xfha8fh.cloudfront.net (108.139.182.16) 56(84) bytes of data.
64 bytes from server-108-139-182-16.gru3.r.cloudfront.net (108.139.182.16): icmp_seq=1 ttl=249 time=11.6 ms
64 bytes from server-108-139-182-16.gru3.r.cloudfront.net (108.139.182.16): icmp_seq=2 ttl=249 time=16.6 ms
64 bytes from server-108-139-182-16.gru3.r.cloudfront.net (108.139.182.16): icmp_seq=3 ttl=249 time=18.7 ms

--- dftex7xfha8fh.cloudfront.net ping statistics ---
29 packets transmitted, 29 received, 0% packet loss, time 28157ms
rtt min/avg/max/mdev = 6.119/13.832/29.224/5.442 ms
";

    #[test]
    fn parses_replies_and_summary() {
        let r = parse_ping(SAMPLE).unwrap();
        assert_eq!(r.host.as_deref(), Some("dftex7xfha8fh.cloudfront.net"));
        assert_eq!(r.ip.as_deref(), Some("108.139.182.16"));
        assert_eq!(r.replies.len(), 3);
        assert_eq!(r.replies[0], Reply { seq: 1, time_ms: 11.6 });
        assert_eq!(r.replies[2].time_ms, 18.7);
        assert_eq!(r.transmitted, 29);
        assert_eq!(r.received, 29);
        assert_eq!(r.loss_pct, 0.0);
        let rtt = r.rtt.unwrap();
        assert_eq!(rtt.min, 6.119);
        assert_eq!(rtt.avg, 13.832);
        assert_eq!(rtt.max, 29.224);
        assert_eq!(rtt.mdev, 5.442);
    }

    #[test]
    fn handles_loss_and_missing_rtt() {
        let out = "PING x (1.1.1.1) 56(84) bytes of data.\n\
--- x ping statistics ---\n\
5 packets transmitted, 0 received, 100% packet loss, time 4100ms\n";
        let r = parse_ping(out).unwrap();
        assert_eq!(r.transmitted, 5);
        assert_eq!(r.received, 0);
        assert_eq!(r.loss_pct, 100.0);
        assert!(r.replies.is_empty());
        assert!(r.rtt.is_none());
    }

    #[test]
    fn fractional_loss_and_replies_only() {
        // A partial capture (Ctrl+C before the summary): replies present, no stats line.
        let out = "64 bytes from h (1.1.1.1): icmp_seq=1 ttl=63 time=0.5 ms\n\
64 bytes from h (1.1.1.1): icmp_seq=2 ttl=63 time=1.25 ms\n";
        let r = parse_ping(out).unwrap();
        assert_eq!(r.replies.len(), 2);
        assert_eq!(r.replies[1].time_ms, 1.25);
    }

    #[test]
    fn non_ping_is_none() {
        assert!(parse_ping("").is_none());
        assert!(parse_ping("total 48\nsome other output\n").is_none());
    }
}
