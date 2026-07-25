//! Headless PTY / process layer (Layer 1 of the terminal — see docs/DESIGN.md §5).
//!
//! Spawns the user's shell on a pseudoterminal with a controlling terminal, pumps
//! bytes in/out, resizes, and reaps the child. It has no knowledge of rendering or
//! IPC, which keeps it unit-testable and reusable across any frontend (the Tauri
//! webview today; a native renderer later).
//!
//! - [`pty`] is the single-session primitive (spawn / write / resize / kill).
//! - [`session`] is the multi-session table the frontend addresses (DESIGN.md §4.3).

pub mod pty;
pub mod session;

pub use pty::{spawn, ExitInfo, PtyEvent, PtyHandle, SpawnConfig};
pub use session::{SessionId, Sessions};
