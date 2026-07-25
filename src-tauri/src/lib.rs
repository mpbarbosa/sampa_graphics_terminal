//! Tauri application shell: bridges the headless cores (`pty-core` Layer 1,
//! `sampa-config` the config model) to the xterm.js frontend over Tauri IPC. This
//! is the only layer that knows about the webview; terminal, session, and config
//! logic all live in headless crates so they stay testable and renderer-agnostic.
//!
//! Protocol (see docs/DESIGN.md §9, §11):
//!   frontend -> backend : `spawn_session`, `write_session`, `resize_session`,
//!                         `close_session`, `get_config`
//!   backend  -> frontend: `pty://output/<id>` (base64 bytes),
//!                         `pty://exit/<id>` ({ code, success, detail }),
//!                         `config://changed` (Config), `config://error` (string)

use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

use pty_core::{PtyEvent, Sessions, SpawnConfig};
use sampa_config::Config;

/// The live configuration (the watcher keeps its own copy of the path).
struct ConfigState {
    current: Mutex<Config>,
}

/// Payload for the `pty://exit/<id>` event.
#[derive(Clone, Serialize)]
struct ExitPayload {
    code: u32,
    success: bool,
    detail: String,
}

/// Start a shell session sized `cols`x`rows`; returns its session id. The shell and
/// its args come from config (`shell.program` else `$SHELL`).
#[tauri::command]
fn spawn_session(
    app: tauri::AppHandle,
    sessions: State<'_, Sessions>,
    config: State<'_, ConfigState>,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    let (tx, rx) = std::sync::mpsc::channel::<PtyEvent>();

    let shell_cfg = config.current.lock().unwrap().shell.clone();
    let shell = shell_cfg
        .program
        .clone()
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut args = shell_cfg.args.clone();
    if shell_cfg.login && !args.iter().any(|a| a == "-l" || a == "--login") {
        args.insert(0, "-l".into());
    }

    let cfg = SpawnConfig {
        shell,
        args,
        cwd: std::env::var("HOME").ok(),
        cols,
        rows,
        env: vec![],
    };
    let id = sessions.spawn(cfg, tx).map_err(|e| e.to_string())?;

    // Pump this session's event stream to the webview. Output bytes may be partial
    // UTF-8 (mid escape sequence), so we base64 them and let xterm.js reassemble.
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
    let _ = sessions.write(session, &bytes);
    Ok(())
}

/// Resize a session (drives SIGWINCH in the shell).
#[tauri::command]
fn resize_session(sessions: State<'_, Sessions>, session: u32, cols: u16, rows: u16) {
    let _ = sessions.resize(session, cols, rows);
}

/// Tear down a session (hang up the shell and reap it).
#[tauri::command]
fn close_session(sessions: State<'_, Sessions>, session: u32) {
    sessions.close(session);
}

/// The current configuration, for the frontend to apply on startup.
#[tauri::command]
fn get_config(config: State<'_, ConfigState>) -> Config {
    config.current.lock().unwrap().clone()
}

/// Resolve the config path (`--config PATH` overrides the XDG default), and load it,
/// writing the documented default to disk on first run so there's something to edit.
fn init_config() -> (Config, Option<PathBuf>) {
    let explicit = std::env::args()
        .skip_while(|a| a != "--config")
        .nth(1)
        .map(PathBuf::from);
    let path = explicit.or_else(sampa_config::default_config_path);

    let Some(p) = path.clone() else {
        return (Config::default(), None);
    };

    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&p, sampa_config::DEFAULT_CONFIG_TOML).is_ok() {
            eprintln!("[sampa] wrote default config to {}", p.display());
        }
    }

    match Config::load(&p) {
        Ok(c) => (c, Some(p)),
        Err(e) => {
            eprintln!("[sampa] config error, using defaults: {e:#}");
            (Config::default(), Some(p))
        }
    }
}

/// Watch the config file's directory and, on edit, reload + broadcast the new config
/// (or an error message) so the frontend can apply it live (DESIGN.md §11).
fn start_config_watcher(app: tauri::AppHandle, path: PathBuf) {
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else {
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[sampa] config watcher init failed: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        eprintln!("[sampa] config watch failed: {e}");
        return;
    }

    std::thread::spawn(move || {
        let _keep = watcher; // dropping the watcher stops it; hold it for the thread's life
        for res in rx {
            let Ok(event) = res else { continue };
            if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                continue;
            }
            // Editors write via temp+rename, so filter the dir events to our file.
            if !event.paths.iter().any(|p| p == &path) {
                continue;
            }
            match Config::load(&path) {
                Ok(cfg) => {
                    if let Some(state) = app.try_state::<ConfigState>() {
                        *state.current.lock().unwrap() = cfg.clone();
                    }
                    let _ = app.emit("config://changed", cfg);
                }
                Err(e) => {
                    let _ = app.emit("config://error", format!("{e:#}"));
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (config, path) = init_config();

    tauri::Builder::default()
        .manage(Sessions::new())
        .manage(ConfigState {
            current: Mutex::new(config),
        })
        .setup(move |app| {
            if let Some(p) = path.clone() {
                start_config_watcher(app.handle().clone(), p);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            spawn_session,
            write_session,
            resize_session,
            close_session,
            get_config
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
