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
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

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

/// A live PTY session. Dropping it lets the child be reaped by the OS; call
/// [`PtyHandle::kill`] for an explicit teardown.
pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
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
        self.child.process_id()
    }

    /// Send SIGHUP/kill to the child.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

/// Spawn `cfg.shell` on a fresh PTY. Output bytes are streamed to `on_output`
/// from a dedicated reader thread; the channel closes when the shell exits.
pub fn spawn(cfg: SpawnConfig, on_output: Sender<Vec<u8>>) -> Result<PtyHandle> {
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

    let child = pair.slave.spawn_command(cmd)?;
    // The parent must not keep the slave open, or read() never sees EOF.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: shell exited
                Ok(n) => {
                    if on_output.send(buf[..n].to_vec()).is_err() {
                        break; // receiver dropped
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(PtyHandle {
        master: pair.master,
        writer,
        child,
    })
}
