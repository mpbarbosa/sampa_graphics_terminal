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

**M1 — a usable terminal (in progress).** Building on the M0 echo scaffold toward the
"real terminal" contract. What's here:

- [x] `pty-core`: spawn shell on a PTY, stream output, write input, resize, reap — **verified** against real zsh via the `echo` example.
- [x] **Session table in the core** (`Sessions`) — id-keyed registry, unit-tested for exit-code capture and close-reaps-child.
- [x] **Exit status**: the `pty://exit/<id>` event now carries `{ code, success, detail }` (clean exit vs. signal), rendered in the terminal.
- [x] **Byte-safe I/O**: input is base64-encoded end-to-end, so control sequences and non-UTF-8 key encodings survive the JS↔Rust boundary.
- [x] Tauri IPC bridge: `spawn_session` / `write_session` / `resize_session` / **`close_session`**.
- [x] xterm.js frontend: 10k scrollback, bracketed paste, `Ctrl-Shift-C`/`Ctrl-Shift-V` clipboard, **multi-line paste confirmation**, teardown on window close.
- [ ] Verify the M1 app matrix (vim/neovim/tmux/htop/less) on a machine with the Tauri system deps — see [docs/ROADMAP.md](docs/ROADMAP.md) M1 exit criteria.
- [ ] Linux integration (`.desktop`, `-e`, default-terminal registration) — M3.
- [ ] The signature features: command palette, live man panel, safe auto-run preview — M4.

The detailed, phased roadmap (M1 → v1) is in **[docs/ROADMAP.md](docs/ROADMAP.md)**
(expanded from the milestone skeleton in [docs/DESIGN.md §18](docs/DESIGN.md)).

## License

MIT
