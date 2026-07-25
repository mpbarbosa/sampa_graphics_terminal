# sampa_graphics_terminal

A graphical terminal for Linux with an automatic **man-page panel** and a **command-line
preview** — a native desktop terminal emulator that runs your real shell (zsh) and layers
on productivity features most terminals lack.

> Design & full implementation guide: **[docs/DESIGN.md](docs/DESIGN.md)**.
> This repository currently contains the **M0 scaffold** (see [Status](#status)).

## Architecture

A headless, testable **core** behind a thin **frontend**, so the renderer can be swapped
later without rewriting the terminal (see DESIGN.md §4):

```
frontend (xterm.js in a Tauri webview)   src/main.ts
        │  Tauri IPC (commands / events)
core    │  Rust
  ├─ pty-core   PTY spawn / read / write / resize / reap   crates/pty-core
  └─ app shell  IPC bridge                                 src-tauri
```

- **`crates/pty-core`** — pure-Rust PTY layer (via `portable-pty`); no GUI dependency, so
  it builds and tests on its own. This is the durable core.
- **`src-tauri`** — the Tauri app: bridges `pty-core` to the webview over IPC.
- **`src/`, `index.html`** — the xterm.js frontend.

## Prerequisites

- **Rust** (stable) + Cargo
- **Node.js** ≥ 18 and npm
- **Tauri Linux system dependencies**, notably **webkit2gtk** and GTK dev libs. On
  Debian/Ubuntu:
  ```sh
  sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  ```
  (See <https://tauri.app/start/prerequisites/> for other distros.)

## Run

```sh
npm install
npm run tauri dev     # launches the native window with a live zsh session
```

Build a release bundle (AppImage/.deb/…):

```sh
npm run tauri icon src-tauri/icons/icon.png   # generate the platform icon set (once)
npm run tauri build
```

### Verify just the PTY core (no GUI needed)

```sh
cargo run --manifest-path crates/pty-core/Cargo.toml --example echo
# spawns your $SHELL, runs a command, prints the output
```

## Status

**M2 — comfort (in progress).** M1 (the "real terminal" contract) is done and merged;
M2 adds configuration. What's here:

- [x] **M1**: core session table, byte-safe I/O, `{ code, success, detail }` exit events, 10k scrollback, bracketed paste + multi-line paste confirmation, `Ctrl-Shift-C/V` clipboard — verified live against zsh/vim/job-control.
- [x] **Config model** (`crates/config`, `sampa-config`): TOML at `$XDG_CONFIG_HOME/sampa/config.toml`, per-field defaults, XDG path, unknown-key rejection, validation, documented default written on first run (6 unit tests).
- [x] **Live reload**: a `notify` file-watcher re-applies theme, font, cursor, scrollback, and padding to a running window on save — no restart (verified live).
- [x] `--config FILE` override; config-driven shell (`shell.program`/`args`/`login`), visual bell.
- [ ] **Tabs, splits, search + keybinding table** (M2 Phase 2.3) — next increment.
- [ ] Linux integration (`.desktop`, `-e`, default-terminal registration) — M3.
- [ ] Signature features: command palette, live man panel, safe auto-run preview — M4.

Config lives at `$XDG_CONFIG_HOME/sampa/config.toml` (created with documented defaults on
first run); edit it and changes apply live.

The detailed, phased roadmap (M1 → v1) is in **[docs/ROADMAP.md](docs/ROADMAP.md)**
(expanded from the milestone skeleton in [docs/DESIGN.md §18](docs/DESIGN.md)).

## License

MIT
