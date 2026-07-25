//! Tauri application shell: bridges the headless `pty-core` (Layer 1) to the
//! xterm.js frontend over Tauri's IPC (commands in, events out). This is the
//! only layer that knows about the webview; all terminal + session behavior lives
//! in `pty-core`, so it can be swapped for a native renderer later.
//!
//! Protocol (see docs/DESIGN.md §9):
//!   frontend -> backend : `spawn_session`, `write_session`, `resize_session`,
//!                         `close_session`
//!   backend  -> frontend: event `pty://output/<id>` (base64 bytes),
//!                         event `pty://exit/<id>`  (`{ code, success, detail }`)

use base64::Engine;
use serde::Serialize;
use tauri::{Emitter, State};

use pty_core::{PtyEvent, Sessions, SpawnConfig};

/// Payload for the `pty://exit/<id>` event.
#[derive(Clone, Serialize)]
struct ExitPayload {
    code: u32,
    success: bool,
    detail: String,
}

/// Start a shell session sized `cols`x`rows`; returns its session id.
#[tauri::command]
fn spawn_session(
    app: tauri::AppHandle,
    sessions: State<'_, Sessions>,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    let (tx, rx) = std::sync::mpsc::channel::<PtyEvent>();

    let cfg = SpawnConfig {
        shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
        args: vec![],
        cwd: std::env::var("HOME").ok(),
        cols,
        rows,
        env: vec![],
    };
    let id = sessions.spawn(cfg, tx).map_err(|e| e.to_string())?;

    // Pump this session's event stream to the webview. Output bytes may be partial
    // UTF-8 (mid escape sequence), so we base64 them and let xterm.js reassemble on
    // the JS side. The loop ends when the shell exits (the stream yields Exit) or
    // the sender is dropped.
    std::thread::spawn(move || {
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Ok(event) = rx.recv() {
            let ok = match event {
                PtyEvent::Output(bytes) => app
                    .emit(&format!("pty://output/{id}"), b64.encode(&bytes))
                    .is_ok(),
                PtyEvent::Exit(info) => {
                    let _ = app.emit(
                        &format!("pty://exit/{id}"),
                        ExitPayload {
                            code: info.code,
                            success: info.success,
                            detail: info.detail,
                        },
                    );
                    break;
                }
            };
            if !ok {
                break; // webview gone
            }
        }
    });

    Ok(id)
}

/// Forward typed/pasted input to a session's shell. `data` is base64 so raw bytes
/// (control chars, non-UTF-8 key encodings) survive the JS↔Rust boundary intact.
#[tauri::command]
fn write_session(sessions: State<'_, Sessions>, session: u32, data: String) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| e.to_string())?;
    // A write to a just-closed session is not an error worth surfacing to the UI.
    let _ = sessions.write(session, &bytes);
    Ok(())
}

/// Resize a session (drives SIGWINCH in the shell).
#[tauri::command]
fn resize_session(
    sessions: State<'_, Sessions>,
    session: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let _ = sessions.resize(session, cols, rows);
    Ok(())
}

/// Tear down a session (hang up the shell and reap it).
#[tauri::command]
fn close_session(sessions: State<'_, Sessions>, session: u32) {
    sessions.close(session);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Sessions::new())
        .invoke_handler(tauri::generate_handler![
            spawn_session,
            write_session,
            resize_session,
            close_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
