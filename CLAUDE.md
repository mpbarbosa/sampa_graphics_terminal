# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`sampa_graphics_terminal` is a native Linux terminal emulator (Tauri + Rust + xterm.js) that runs the user's real shell and aims to add a live man-page panel, command palette, and safe auto-run preview. It is currently at the **M0 scaffold** stage: PTY spawn/read/write/resize/reap works end-to-end (native window ↔ zsh), but the signature productivity features are not built yet. See `docs/DESIGN.md` for the full design and the M1→v1 roadmap.

## Commands

```sh
npm install                    # install frontend deps (first time)
npm run tauri dev              # run the native app with a live shell (main dev loop)
npm run tauri build            # release bundle (AppImage/.deb); run `npm run tauri icon src-tauri/icons/icon.png` once first
npm run build                  # frontend typecheck + vite build (tsc --noEmit && vite build)

# Verify Layer 1 (PTY core) with no GUI — fastest feedback loop for core changes:
cargo run --manifest-path crates/pty-core/Cargo.toml --example echo

# Headless core crates (fast; no GUI/system deps needed):
cargo test  --manifest-path crates/pty-core/Cargo.toml    # PTY layer (session table, exit reaping)
cargo test  --manifest-path crates/config/Cargo.toml      # config model (defaults, TOML, validation)
cargo build --manifest-path src-tauri/Cargo.toml          # the Tauri app crate (needs GTK/webkit deps)
```

The headless crates (`pty-core`, `config`) have unit tests and build without the GUI toolchain — use them as the fast inner loop. `npm run dev` (vite alone) has no Tauri IPC backend — always use `npm run tauri dev` to exercise the real app.

Building the Tauri app requires the Linux system deps in `README.md` (webkit2gtk, GTK dev libs, etc.).

## Architecture

The core design principle (DESIGN.md §4) is a **renderer-agnostic core**: a headless, testable Rust core behind a thin frontend, so the webview renderer can be swapped for a native GPU renderer later *without rewriting the terminal*. Keep this seam clean — do not leak GUI/webview concerns into the core.

Four layers across two halves:

- **Layer 1 — PTY/process** (`crates/pty-core/`): pure-Rust, **zero GUI dependency** so it builds and tests standalone. Spawns the shell on a PTY via `portable-pty` (which handles the POSIX openpty/setsid/TIOCSCTTY dance, so job control and terminal signals work). Streams output bytes over an `mpsc::Sender` from a dedicated reader thread. This is the durable, long-lived asset. **Keep it free of any Tauri/webview imports.**
- **Config model** (`crates/config/`, `sampa-config`): another headless, GUI-free crate — serde/TOML `Config` with per-field defaults and XDG path resolution (DESIGN.md §11). Same rule as `pty-core`: no Tauri/webview imports.
- **App shell** (`src-tauri/`): the only layer that knows about the webview. Bridges the headless crates to the frontend over Tauri IPC. Owns the `Sessions` table (from `pty-core`) and a `ConfigState`, plus a `notify` file-watcher that emits `config://changed` on config edits. Depends on `pty-core` and `sampa-config` by path.
- **Layers 3–4 — renderer + input** (`src/main.ts`, `index.html`): xterm.js in the Tauri webview. Deliberately thin — all terminal behavior belongs in the core. Fetches `get_config` on load and maps `Config` onto xterm options; re-applies on the `config://changed` event.

### The IPC seam (DESIGN.md §9)

The frontend↔core protocol is intentionally small and symmetric (so a future native frontend uses the same *shape* as a function-call API):

- **Commands (frontend → backend):** `spawn_session(cols, rows) -> id`, `write_session(session, data)`, `resize_session(session, cols, rows)`.
- **Events (backend → frontend):** `pty://output/<id>` and `pty://exit/<id>`.

**Output is base64-encoded across the IPC boundary.** PTY output is raw bytes that may split mid-UTF-8 / mid-escape-sequence, so the backend base64-encodes each chunk and the frontend decodes to a `Uint8Array` and lets xterm.js reassemble. Do not treat PTY output as UTF-8 strings at the boundary.

### Conventions when extending

- New terminal behavior (VT handling, services like the man panel / palette / preview / config / sessions) goes in the **core**, not the frontend. The frontend only renders and captures input.
- New IPC calls follow the command/event split above and go through `src-tauri/src/lib.rs`'s `invoke_handler`.
- Section references in code comments (`DESIGN.md §5.1`, `§9`, etc.) point at the authoritative design doc — consult it before changing PTY lifecycle, resize/SIGWINCH, or the protocol.
