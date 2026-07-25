//! Session table (DESIGN.md §4.3): a headless registry of live PTY sessions
//! keyed by a small integer id. This is the substrate the frontend addresses
//! sessions through, and the thing tabs (M2) are built on. It owns no rendering
//! or IPC concerns — the caller supplies a channel per session and pumps the
//! resulting [`PtyEvent`] stream wherever it likes.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use anyhow::{anyhow, Result};

use crate::pty::{spawn, PtyEvent, PtyHandle, SpawnConfig};

/// Opaque session identifier. Ids are never reused within a process.
pub type SessionId = u32;

/// A thread-safe table of live sessions. Cheap to share behind a reference
/// (e.g. Tauri's managed state); all methods take `&self`.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    map: HashMap<SessionId, PtyHandle>,
    next_id: SessionId,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a shell per `cfg`, streaming its events to `on_event`, and register
    /// it. Returns the new session id.
    pub fn spawn(&self, cfg: SpawnConfig, on_event: Sender<PtyEvent>) -> Result<SessionId> {
        let handle = spawn(cfg, on_event)?;
        let mut inner = self.inner.lock().unwrap();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.map.insert(id, handle);
        Ok(id)
    }

    /// Forward bytes to a session's shell. Errors if the id is unknown.
    pub fn write(&self, id: SessionId, data: &[u8]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let h = inner.map.get_mut(&id).ok_or_else(|| unknown(id))?;
        h.write(data).map_err(Into::into)
    }

    /// Resize a session (drives SIGWINCH). Errors if the id is unknown.
    pub fn resize(&self, id: SessionId, cols: u16, rows: u16) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let h = inner.map.get(&id).ok_or_else(|| unknown(id))?;
        h.resize(cols, rows, 0, 0)
    }

    /// The shell pid for a session, if known.
    pub fn pid(&self, id: SessionId) -> Option<u32> {
        self.inner.lock().unwrap().map.get(&id).and_then(|h| h.pid())
    }

    /// Hang up and remove a session. Returns `true` if it existed. The child is
    /// reaped by its own reader thread once the SIGHUP lands, so no zombie is
    /// left behind. Idempotent: closing an unknown id is a no-op returning
    /// `false`.
    pub fn close(&self, id: SessionId) -> bool {
        let mut handle = match self.inner.lock().unwrap().map.remove(&id) {
            Some(h) => h,
            None => return false,
        };
        // Best-effort hang-up; ignore errors (e.g. the shell already exited).
        let _ = handle.kill();
        true
    }

    /// Number of live sessions.
    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }
}

fn unknown(id: SessionId) -> anyhow::Error {
    anyhow!("unknown session {id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    /// Collect events until Exit (or timeout), returning (all output bytes, exit).
    fn drain(rx: std::sync::mpsc::Receiver<PtyEvent>) -> (Vec<u8>, Option<crate::pty::ExitInfo>) {
        let mut out = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(PtyEvent::Output(b)) => out.extend_from_slice(&b),
                Ok(PtyEvent::Exit(info)) => return (out, Some(info)),
                Err(_) => return (out, None),
            }
        }
    }

    fn sh_config(script: &str) -> SpawnConfig {
        SpawnConfig {
            shell: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            cols: 80,
            rows: 24,
            env: vec![],
        }
    }

    #[test]
    fn reports_output_then_exit_code() {
        let sessions = Sessions::new();
        let (tx, rx) = channel();
        let id = sessions
            .spawn(sh_config("printf 'hello'; exit 7"), tx)
            .unwrap();
        assert!(id > 0);

        let (out, exit) = drain(rx);
        assert!(
            String::from_utf8_lossy(&out).contains("hello"),
            "expected shell output, got {:?}",
            String::from_utf8_lossy(&out)
        );
        let exit = exit.expect("should receive an Exit event");
        assert_eq!(exit.code, 7);
        assert!(!exit.success);

        // The reader thread reaps and drops nothing from the table, but the shell
        // is gone; close() on the still-registered id is harmless.
        sessions.close(id);
    }

    #[test]
    fn close_reaps_a_live_shell() {
        let sessions = Sessions::new();
        let (tx, rx) = channel();
        // An interactive shell reading its stdin: it won't exit on its own.
        let id = sessions.spawn(sh_config("cat"), tx).unwrap();
        assert_eq!(sessions.count(), 1);

        assert!(sessions.close(id), "close should report the session existed");
        assert_eq!(sessions.count(), 0);
        assert!(!sessions.close(id), "second close is a no-op");

        // The SIGHUP must drive the session to a terminal Exit event.
        let (_out, exit) = drain(rx);
        assert!(exit.is_some(), "closed session must still emit Exit");
    }

    #[test]
    fn write_and_resize_reject_unknown_ids() {
        let sessions = Sessions::new();
        assert!(sessions.write(999, b"x").is_err());
        assert!(sessions.resize(999, 80, 24).is_err());
        assert_eq!(sessions.pid(999), None);
    }
}
