//! Tauri application shell: bridges the headless `pty-core` (Layer 1) to the
//! xterm.js frontend over Tauri's IPC (commands in, events out). This is the
//! only layer that knows about the webview; all terminal behavior lives in
//! `pty-core`, so it can be swapped for a native renderer later.
//!
//! Protocol (see docs/DESIGN.md §9):
//!   frontend -> backend : `spawn_session`, `write_session`, `resize_session`
//!   backend  -> frontend: event `pty://output/<id>` (base64 bytes),
//!                         event `pty://exit/<id>`

use std::collections::HashMap;
use std::sync::mpsc::channel;
use std::sync::Mutex;

use base64::Engine;
use tauri::{Emitter, State};

use pty_core::{spawn, PtyHandle, SpawnConfig};

#[derive(Default)]
struct AppState {
    sessions: Mutex<HashMap<u32, PtyHandle>>,
    next_id: Mutex<u32>,
}

/// Start a shell session sized `cols`x`rows`; returns its session id.
#[tauri::command]
fn spawn_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    let (tx, rx) = channel::<Vec<u8>>();

    let cfg = SpawnConfig {
        shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
        args: vec![],
        cwd: std::env::var("HOME").ok(),
        cols,
        rows,
        env: vec![],
    };
    let handle = spawn(cfg, tx).map_err(|e| e.to_string())?;

    let id = {
        let mut n = state.next_id.lock().unwrap();
        *n += 1;
        *n
    };
    state.sessions.lock().unwrap().insert(id, handle);

    // Pump PTY output to the webview. Bytes may be partial UTF-8 (mid escape
    // sequence), so we base64 them and let xterm.js reassemble on the JS side.
    std::thread::spawn(move || {
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Ok(bytes) = rx.recv() {
            if app
                .emit(&format!("pty://output/{id}"), b64.encode(&bytes))
                .is_err()
            {
                break;
            }
        }
        let _ = app.emit(&format!("pty://exit/{id}"), ());
    });

    Ok(id)
}

/// Forward typed/pasted input to a session's shell.
#[tauri::command]
fn write_session(state: State<'_, AppState>, session: u32, data: String) -> Result<(), String> {
    if let Some(h) = state.sessions.lock().unwrap().get_mut(&session) {
        h.write(data.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resize a session (drives SIGWINCH in the shell).
#[tauri::command]
fn resize_session(
    state: State<'_, AppState>,
    session: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    if let Some(h) = state.sessions.lock().unwrap().get(&session) {
        h.resize(cols, rows, 0, 0).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            spawn_session,
            write_session,
            resize_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
