//! Parse `ss(8)` socket output into a connections table for the netstat decorator.
//!
//! When the user has typed `netstat` (or `ss`) and presses the enhance shortcut, the bridge
//! runs `ss -tunap` and hands its output here. `netstat` (net-tools) is deprecated and often
//! absent on modern Linux; `ss` is its replacement and has a uniform column layout (unlike
//! netstat, whose udp rows drop the State column), so it's the parse target. `parse_ss`
//! yields one [`Conn`] per socket — protocol, state, local/peer address, and the owning
//! process — for the frontend to render as a table. Informational; nothing is run.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: input that isn't an `ss`
//! table yields `None`, mirroring the other decorator cores.

use serde::{Deserialize, Serialize};

/// One socket from `ss -tunap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conn {
    /// `tcp` / `udp` (ss's Netid).
    pub proto: String,
    /// e.g. `LISTEN`, `ESTAB`, `UNCONN`, `TIME-WAIT`.
    pub state: String,
    /// `addr:port` (IPv6 kept intact as `ss` prints it).
    pub local: String,
    pub peer: String,
    /// Owning process as `name (pid)`, when `ss -p` reported one (needs privilege for
    /// other users' sockets).
    pub process: Option<String>,
}

/// Parse `ss -tunap` output. The header line (`Netid State … Local Address:Port …`) is
/// required; each row is `netid state recvq sendq local peer [process…]`. `None` if the
/// header isn't `ss`'s or no rows parse.
pub fn parse_ss(output: &str) -> Option<Vec<Conn>> {
    let mut lines = output.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    // ss headers start with "Netid" (-t/-u) or "State" (single-family); require the address
    // columns so arbitrary text isn't mistaken for ss output.
    if !((header.starts_with("Netid") || header.starts_with("State"))
        && header.contains("Local Address"))
    {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 6 {
            continue;
        }
        out.push(Conn {
            proto: t[0].to_string(),
            state: t[1].to_string(),
            local: t[4].to_string(),
            peer: t[5].to_string(),
            process: t.get(6..).map(|rest| rest.join(" ")).and_then(|s| extract_process(&s)),
        });
    }
    (!out.is_empty()).then_some(out)
}

/// Pull a friendly `name (pid)` from ss's process column,
/// e.g. `users:(("chrome",pid=240425,fd=40))` → `chrome (240425)`. `None` if empty/unmatched.
fn extract_process(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Name is the first double-quoted token; pid follows `pid=`.
    let name = s.split('"').nth(1);
    let pid = s.split("pid=").nth(1).map(|rest| {
        rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>()
    });
    match (name, pid) {
        (Some(n), Some(p)) if !p.is_empty() => Some(format!("{n} ({p})")),
        (Some(n), _) => Some(n.to_string()),
        _ => Some(s.to_string()), // unrecognised shape — show it raw rather than drop it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Netid State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process
udp   ESTAB  0      0          192.168.68.112:45746   160.79.104.10:443 users:((\"claude-desktop\",pid=6225,fd=25))
udp   UNCONN 0      0                 0.0.0.0:48521          0.0.0.0:*   users:((\"python3\",pid=18880,fd=12))
tcp   LISTEN 0      128                0.0.0.0:22             0.0.0.0:*   users:((\"sshd\",pid=900,fd=3))
tcp   ESTAB  0      0          192.168.68.112:54321   140.82.113.25:443
";

    #[test]
    fn parses_connections() {
        let conns = parse_ss(SAMPLE).unwrap();
        assert_eq!(conns.len(), 4);
        assert_eq!(conns[0].proto, "udp");
        assert_eq!(conns[0].state, "ESTAB");
        assert_eq!(conns[0].local, "192.168.68.112:45746");
        assert_eq!(conns[0].peer, "160.79.104.10:443");
        assert_eq!(conns[0].process.as_deref(), Some("claude-desktop (6225)"));
        assert_eq!(conns[2].proto, "tcp");
        assert_eq!(conns[2].state, "LISTEN");
        assert_eq!(conns[2].process.as_deref(), Some("sshd (900)"));
        // A row with no process column (non-root / no -p match).
        assert_eq!(conns[3].process, None);
    }

    #[test]
    fn non_ss_is_none() {
        assert!(parse_ss("").is_none());
        assert!(parse_ss("total 48\nsome text\n").is_none());
        // netstat's own header isn't ss's — we only parse ss output.
        assert!(parse_ss("Proto Recv-Q Send-Q Local Address Foreign Address State\n").is_none());
    }
}
