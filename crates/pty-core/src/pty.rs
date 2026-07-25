//! PTY spawn / read / write / resize / reap, built on `portable-pty`.
//!
//! `portable-pty` performs the POSIX dance described in DESIGN.md §5.1
//! (openpty + grantpt/unlockpt, fork, setsid, TIOCSCTTY, exec) so the child gets
//! a real controlling terminal — meaning job control and terminal signals
//! (Ctrl-C → SIGINT, Ctrl-Z → SIGTSTP, SIGWINCH on resize) work for free.

use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::Result;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// How to start a session.
pub struct SpawnConfig {
    /// Shell binary, e.g. `/usr/bin/zsh`.
    pub shell: String,
    /// Extra argv (e.g. `-l` for a login shell).
    pub args: Vec<String>,
    /// Working directory; `None` inherits the process cwd.
    pub cwd: Option<String>,
    /// Initial grid size.
    pub cols: u16,
    pub rows: u16,
    /// Additional environment overrides, applied after the terminal defaults.
    pub env: Vec<(String, String)>,
}

/// How a session ended. Reported once, after all output has been drained.
///
/// `portable-pty` flattens the platform wait-status into an exit `code` plus a
/// success flag; it does not expose the raw signal number through its public
/// API, so a signal death shows up as `success == false` with `detail` naming
/// the signal (e.g. `"Terminated by Interrupt"`).
#[derive(Debug, Clone)]
pub struct ExitInfo {
    /// Process exit code (1 when terminated by a signal).
    pub code: u32,
    /// True only for a clean, code-0, non-signal exit.
    pub success: bool,
    /// Human-readable status: `"Success"`, `"Exited with code 3"`, or
    /// `"Terminated by <signal>"`.
    pub detail: String,
}

/// A single item in a session's event stream. Ordering is guaranteed: every
/// [`PtyEvent::Output`] a session will ever produce is delivered before its
/// terminal [`PtyEvent::Exit`], because both come from the same reader thread.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Raw bytes from the shell. May split mid-UTF-8 / mid-escape-sequence — do
    /// not assume a chunk is a valid string.
    Output(Vec<u8>),
    /// The shell exited; the stream is finished.
    Exit(ExitInfo),
}

/// A live PTY session. Dropping it closes the master, but a shell whose reader
/// clone is still open (it always is, in the reader thread) only receives its
/// hang-up when [`PtyHandle::kill`] signals it — so explicit teardown goes
/// through `kill`, not `drop`.
pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    /// Signals the child independently of the reader thread's blocking `wait`.
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
}

impl PtyHandle {
    /// Forward bytes (typed input, pasted text) to the shell.
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// Resize the terminal; the kernel delivers SIGWINCH to the foreground group.
    /// `xpixel`/`ypixel` are reported for programs that need pixel geometry
    /// (e.g. sixel/kitty image protocols); 0 is acceptable when unknown.
    pub fn resize(&self, cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: xpixel,
            pixel_height: ypixel,
        })?;
        Ok(())
    }

    /// The shell's process id, if known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Hang up the child (SIGHUP). The shell exits, the reader thread then sees
    /// EOF and reaps it — so this drives the session's final `Exit` event.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.killer.kill()
    }
}

/// Spawn `cfg.shell` on a fresh PTY. Events (`Output`, then a terminal `Exit`)
/// are streamed to `on_event` from a dedicated reader thread; the channel closes
/// when that thread finishes.
pub fn spawn(cfg: SpawnConfig, on_event: Sender<PtyEvent>) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: cfg.rows,
        cols: cfg.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&cfg.shell);
    for arg in &cfg.args {
        cmd.arg(arg);
    }
    if let Some(cwd) = &cfg.cwd {
        cmd.cwd(cwd);
    }
    // Terminal defaults first; caller overrides can still replace these.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "sampa-terminal");
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }

    let mut child = pair.slave.spawn_command(cmd)?;
    // The parent must not keep the slave open, or read() never sees EOF.
    drop(pair.slave);

    let pid = child.process_id();
    // A killer we can signal from elsewhere while the reader thread blocks in wait().
    let killer = child.clone_killer();

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    // One thread owns the child and drains output to EOF, *then* reaps. Doing both
    // here (rather than a separate waiter thread) is what guarantees every Output
    // is sent before Exit.
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: shell exited
                Ok(n) => {
                    if on_event.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                        return; // receiver dropped; don't bother reaping into the void
                    }
                }
                Err(_) => break,
            }
        }
        // EOF reached: the child is exiting; wait() returns its status promptly.
        let info = match child.wait() {
            Ok(status) => ExitInfo {
                code: status.exit_code(),
                success: status.success(),
                detail: status.to_string(),
            },
            Err(e) => ExitInfo {
                code: 1,
                success: false,
                detail: format!("wait failed: {e}"),
            },
        };
        let _ = on_event.send(PtyEvent::Exit(info));
    });

    Ok(PtyHandle {
        master: pair.master,
        writer,
        killer,
        pid,
    })
}
