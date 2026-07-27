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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use base64::Engine;
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

use pty_core::{PtyEvent, Sessions, SpawnConfig};
use sampa_cli::CliArgs;
use sampa_config::Config;
use sampa_shellint::{OscScanner, ShellEvent};

/// Per-session shell-integration state (DESIGN.md §5.6). Populated from OSC 7/133 in
/// the output pump; queried by the cwd-aware features. Shared behind an `Arc` so the
/// pump threads and the command handlers see the same map.
#[derive(Default, Clone)]
struct ShellStates(Arc<Mutex<HashMap<u32, ShellState>>>);

#[derive(Default)]
struct ShellState {
    /// Working directory reported via OSC 7 (authoritative when present).
    cwd: Option<String>,
}

/// An OSC 133 prompt/command mark, forwarded to the frontend for the M4 features.
#[derive(Clone, Serialize)]
struct ShellMark {
    kind: &'static str,
    exit_code: Option<i32>,
}

/// Lazily-computed, cached list of `$PATH` executables for the command palette.
/// `$PATH` is fixed for the app process, so a one-shot scan is enough.
#[derive(Default)]
struct CommandCache(Mutex<Option<Vec<String>>>);

/// The live configuration (the watcher keeps its own copy of the path).
struct ConfigState {
    current: Mutex<Config>,
}

/// Parsed command line, plus a one-shot flag: the CLI overrides (`-e`, cwd, login)
/// apply only to the *first* session, so new tabs open a normal shell.
struct CliState {
    args: CliArgs,
    first_spawn: AtomicBool,
}

/// Launch-time options the frontend needs (`--hold`, `--title`).
#[derive(Clone, Serialize)]
struct LaunchOptions {
    hold: bool,
    title: Option<String>,
    exec: bool,
}

/// Per-session "start pumping" gates. `spawn_session` parks each session's output
/// pump on its gate until the frontend has attached its listeners and calls
/// `session_ready` — otherwise a fast `-e` command can exit before anyone is
/// listening and its output is lost. Buffered PTY output waits in the channel
/// meanwhile, so nothing is dropped.
#[derive(Default)]
struct ReadyGate {
    map: Mutex<HashMap<u32, Sender<()>>>,
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
    cli: State<'_, CliState>,
    ready: State<'_, ReadyGate>,
    shell_states: State<'_, ShellStates>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> Result<u32, String> {
    let (tx, rx) = std::sync::mpsc::channel::<PtyEvent>();

    let shell_cfg = config.current.lock().unwrap().shell.clone();
    // CLI overrides (-e/cwd/login) apply only to the first session.
    let apply_cli = cli.first_spawn.swap(false, Ordering::SeqCst);

    let (shell, args) = match apply_cli.then(|| cli.args.exec.clone()).flatten() {
        // `-e CMD ARGS…`: run the command instead of the shell (argv is non-empty).
        Some(exec) => (exec[0].clone(), exec[1..].to_vec()),
        None => {
            let program = shell_cfg
                .program
                .clone()
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "/bin/zsh".into());
            let mut a = shell_cfg.args.clone();
            let login = shell_cfg.login || (apply_cli && cli.args.login);
            if login && !a.iter().any(|x| x == "-l" || x == "--login") {
                a.insert(0, "-l".into());
            }
            (program, a)
        }
    };

    // cwd precedence: CLI --working-directory (first session) > frontend hint (a new
    // tab inheriting the active tab's cwd) > $HOME.
    let cwd = apply_cli
        .then(|| cli.args.working_directory.clone())
        .flatten()
        .or(cwd)
        .or_else(|| std::env::var("HOME").ok());

    let cfg = SpawnConfig {
        shell,
        args,
        cwd,
        cols,
        rows,
        env: vec![],
    };
    let id = sessions.spawn(cfg, tx).map_err(|e| e.to_string())?;

    // Gate the pump until the frontend attaches listeners (see ReadyGate).
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    ready.map.lock().unwrap().insert(id, ready_tx);

    let states = shell_states.0.clone();

    // Pump this session's event stream to the webview. Output bytes may be partial
    // UTF-8 (mid escape sequence), so we base64 them and let xterm.js reassemble. We
    // also scan the stream for OSC 7/133 shell-integration signals (DESIGN.md §5.6).
    std::thread::spawn(move || {
        // Wait until the frontend is listening (or the gate is dropped).
        let _ = ready_rx.recv();
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut scanner = OscScanner::new();
        while let Ok(event) = rx.recv() {
            let ok = match event {
                PtyEvent::Output(bytes) => {
                    for ev in scanner.feed(&bytes) {
                        match ev {
                            ShellEvent::Cwd(path) => {
                                states.lock().unwrap().entry(id).or_default().cwd =
                                    Some(path.clone());
                                let _ = app.emit(&format!("shell://cwd/{id}"), path);
                            }
                            ShellEvent::PromptStart => emit_mark(&app, id, "prompt-start", None),
                            ShellEvent::CommandStart => emit_mark(&app, id, "command-start", None),
                            ShellEvent::OutputStart => emit_mark(&app, id, "output-start", None),
                            ShellEvent::CommandEnd(code) => emit_mark(&app, id, "command-end", code),
                        }
                    }
                    app.emit(&format!("pty://output/{id}"), b64.encode(&bytes))
                        .is_ok()
                }
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
        states.lock().unwrap().remove(&id);
    });

    Ok(id)
}

fn emit_mark(app: &tauri::AppHandle, id: u32, kind: &'static str, exit_code: Option<i32>) {
    let _ = app.emit(&format!("shell://mark/{id}"), ShellMark { kind, exit_code });
}

/// The shell's cwd for `pid`, from `/proc/<pid>/cwd` (Linux fallback when OSC 7 is
/// unavailable, e.g. the shell integration isn't sourced).
fn proc_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .and_then(|p| p.to_str().map(String::from))
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

/// The frontend has attached its listeners for `session`; release its output pump.
#[tauri::command]
fn session_ready(ready: State<'_, ReadyGate>, session: u32) {
    if let Some(tx) = ready.map.lock().unwrap().remove(&session) {
        let _ = tx.send(());
    }
}

/// The session's working directory: OSC 7 if the shell reported it, else
/// `/proc/<pid>/cwd`. Used to open new tabs in the same directory and (in M4) to run
/// the man/preview services in the session's cwd.
#[tauri::command]
fn get_session_cwd(
    sessions: State<'_, Sessions>,
    shell_states: State<'_, ShellStates>,
    session: u32,
) -> Option<String> {
    if let Some(cwd) = shell_states
        .0
        .lock()
        .unwrap()
        .get(&session)
        .and_then(|s| s.cwd.clone())
    {
        return Some(cwd);
    }
    sessions.pid(session).and_then(proc_cwd)
}

/// The current configuration, for the frontend to apply on startup.
#[tauri::command]
fn get_config(config: State<'_, ConfigState>) -> Config {
    config.current.lock().unwrap().clone()
}

/// Executables on `$PATH`, for the command palette (DESIGN.md §10.1). Computed once
/// and cached.
#[tauri::command]
fn list_commands(cache: State<'_, CommandCache>) -> Vec<String> {
    let mut guard = cache.0.lock().unwrap();
    if guard.is_none() {
        let path = std::env::var("PATH").unwrap_or_default();
        *guard = Some(sampa_palette::list_executables(&path));
    }
    guard.clone().unwrap_or_default()
}

/// Quit the app (used when the last tab is closed).
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Launch-time options from the command line (`--hold`, `--title`).
#[tauri::command]
fn get_launch_options(cli: State<'_, CliState>) -> LaunchOptions {
    LaunchOptions {
        hold: cli.args.hold,
        title: cli.args.title.clone(),
        exec: cli.args.exec.is_some(),
    }
}

/// Load the config from `explicit` (else the XDG default), writing the documented
/// default to disk on first run so there's something to edit.
fn init_config(explicit: Option<PathBuf>) -> (Config, Option<PathBuf>) {
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
    let cli = sampa_cli::parse(&std::env::args().skip(1).collect::<Vec<_>>());
    if cli.help {
        print!("{}", sampa_cli::HELP);
        std::process::exit(0);
    }
    if cli.version {
        println!("sampa {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    for w in &cli.warnings {
        eprintln!("[sampa] {w}");
    }

    let (config, path) = init_config(cli.config.clone().map(PathBuf::from));
    let title = cli.title.clone();

    tauri::Builder::default()
        .manage(Sessions::new())
        .manage(ReadyGate::default())
        .manage(ShellStates::default())
        .manage(CommandCache::default())
        .manage(ConfigState {
            current: Mutex::new(config),
        })
        .manage(CliState {
            args: cli,
            first_spawn: AtomicBool::new(true),
        })
        .setup(move |app| {
            if let Some(t) = &title {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_title(t);
                }
            }
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
            session_ready,
            get_session_cwd,
            get_config,
            list_commands,
            quit_app,
            get_launch_options
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
