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
npm run tauri build                            # all configured targets
npm run tauri build -- --bundles deb           # just the .deb
```

Install the `.deb` and make Sampa a registered terminal:

```sh
sudo dpkg -i "src-tauri/target/release/bundle/deb/Sampa Terminal_0.1.0_amd64.deb"
# postinst runs: update-alternatives --install /usr/bin/x-terminal-emulator … /usr/bin/sampa 50
sampa                        # the installed command
x-terminal-emulator -e htop  # launchers that use this will now open Sampa
```

The `.desktop` entry carries `Categories=…;TerminalEmulator;`, so `xdg-terminal-exec`-aware
launchers discover it too. **GNOME** removed its default-terminal setting; there GNOME users
rely on the `xdg-terminal-exec` path or per-app settings (no one-click default).

### Verify just the PTY core (no GUI needed)

```sh
cargo run --manifest-path crates/pty-core/Cargo.toml --example echo
# spawns your $SHELL, runs a command, prints the output
```

## Status

**M4 — signature features (done).** M0–M4 are complete: the real-terminal contract,
configuration + tabs, Linux integration, and the signature features (shell integration,
command palette, live man panel, safe preview). What's here:

- [x] **M1**: core session table, byte-safe I/O, `{ code, success, detail }` exit events, scrollback, bracketed paste + multi-line paste confirmation, clipboard — verified live against zsh/vim/job-control.
- [x] **Config model** (`crates/config`, `sampa-config`): TOML at `$XDG_CONFIG_HOME/sampa/config.toml`, per-field defaults, XDG path, unknown-key rejection, validation, documented default on first run (7 unit tests).
- [x] **Live reload**: a `notify` file-watcher re-applies theme, font, cursor, scrollback, and padding to a running window on save — no restart.
- [x] **Tabs**: config-driven keybindings (`Ctrl+Shift+T/W/←/→`), OSC-titled tabs, chrome-free single tab, close reaps the child, last-tab-close quits.
- [x] **Search** (`Ctrl+Shift+F`) with incremental highlight; font zoom; config-driven shell; visual bell.
- [x] **CLI contract** (M3): `-e CMD` / `-- CMD`, `--working-directory`, `--title`, `--hold`, `--login`, `--class`, `--config` — verified live.
- [x] **`.deb` packaging** (M3): `sampa` binary + `.desktop` (TerminalEmulator category); `postinst` registers `x-terminal-emulator` via `update-alternatives` — verified with `dpkg-deb`.
- [x] **Shell integration** (M4 Phase 4.0): OSC 7 cwd + OSC 133 semantic-prompt scanning (`crates/shellint`); per-session cwd tracking (OSC 7 else `/proc`), new tabs inherit the active cwd; opt-in `shell-integration/sampa.{zsh,bash}` hooks.
- [x] **Command palette** (M4 Phase 4.1, `Ctrl+Shift+P`): fuzzy search over `$PATH` executables (`crates/palette`) that **inserts** the command at the prompt — never auto-runs.
- [x] **Live man panel** (M4 Phase 4.2, `Ctrl+Shift+M` / `features.man`): as you type a command, shows its `man` page in a side panel (`crates/man`; `man -P cat`, no shell, sanitized); collapses for keywords/no-man.
- [x] **Safe auto-run preview** (M4 Phase 4.3, `Ctrl+Shift+R` / `features.preview`): as you type a **read-only** command, previews its output in a bottom panel — via a core allowlist gate (`crates/preview`) that runs it in the cwd with a timeout, closed stdin, and output cap. A typed `rm`/`>`/`find -delete` is filesystem-verified to never run.
- [x] **Rendering & graphics** (M5): WebGL GPU renderer (canvas fallback), inline **sixel/iTerm images** (`[rendering] images`), and clickable **hyperlinks** (plain + OSC 8) opened only after a confirmation showing the target.

Config lives at `$XDG_CONFIG_HOME/sampa/config.toml` (created with documented defaults on
first run); edit it and changes apply live. Keybindings are in the `[keybindings]` section.

### Command line

```sh
sampa                                  # open with your $SHELL
sampa -e htop                          # run a command instead of the shell
sampa -- ls -la                        # same, `--` form
sampa --working-directory=/tmp         # start in a directory
sampa --title "Build" --hold -e make   # titled window that stays open after make exits
sampa --config ~/other.toml            # use a specific config
sampa --help
```

### Shell integration (optional)

Source the hook so the shell reports its cwd (OSC 7) and prompt boundaries (OSC 133),
which sharpens the M4 features. It's a no-op outside Sampa:

```sh
# ~/.zshrc
source /path/to/sampa/shell-integration/sampa.zsh
# ~/.bashrc
source /path/to/sampa/shell-integration/sampa.bash
```

Without it, Sampa still tracks the working directory via `/proc/<pid>/cwd`.

The detailed, phased roadmap (M1 → v1) is in **[docs/ROADMAP.md](docs/ROADMAP.md)**
(expanded from the milestone skeleton in [docs/DESIGN.md §18](docs/DESIGN.md)).

## License

MIT
