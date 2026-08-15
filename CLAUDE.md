# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`sampa_graphics_terminal` is a native Linux terminal emulator (Tauri + Rust + xterm.js) that runs the user's real shell and aims to add a live man-page panel, command palette, and safe auto-run preview. It is currently at the **M0 scaffold** stage: PTY spawn/read/write/resize/reap works end-to-end (native window ↔ zsh), but the signature productivity features are not built yet. See `docs/DESIGN.md` for the full design and the M1→v1 roadmap.

## Commands

```sh
npm install                    # install frontend deps (first time)
npm run tauri dev              # run the native app with a live shell (main dev loop)
npm run tauri build            # release bundle (deb/rpm/AppImage); run `npm run tauri icon src-tauri/icons/icon.png` once first
npm run tauri build -- --bundles rpm  # just the .rpm (native Tauri builder, no rpmbuild needed)
npm run build                  # frontend typecheck + vite build (tsc --noEmit && vite build)

# Verify Layer 1 (PTY core) with no GUI — fastest feedback loop for core changes:
cargo run --manifest-path crates/pty-core/Cargo.toml --example echo

# Headless core crates (fast; no GUI/system deps needed):
cargo test  --manifest-path crates/pty-core/Cargo.toml    # PTY layer (session table, exit reaping)
cargo test  --manifest-path crates/config/Cargo.toml      # config model (defaults, TOML, validation)
cargo test  --manifest-path crates/cli/Cargo.toml         # argv parser (-e/--working-directory/--hold/…)
cargo test  --manifest-path crates/shellint/Cargo.toml    # OSC 7/133 shell-integration scanner
cargo test  --manifest-path crates/palette/Cargo.toml     # $PATH executable enumeration (command palette)
cargo test  --manifest-path crates/man/Cargo.toml         # man validate/sanitize (live man panel)
cargo test  --manifest-path crates/preview/Cargo.toml     # preview allowlist gate (incl. filesystem rm-safety test)
cargo test  --manifest-path crates/ai/Cargo.toml          # NL→command request-build/parse (fake transport; no network)
cargo test  --manifest-path crates/ps-decorate/Cargo.toml # ps(1) header-match + 1a decorate (parser fails safe to raw)
cargo test  --manifest-path crates/fsnav/Cargo.toml       # cd tree: list_subdirs/relativize (temp-dir fixtures)
cargo test  --manifest-path crates/dumap/Cargo.toml       # du treemap: parse_du builds a sized tree (fails safe to None)
ps aux | cargo run --example decorate --manifest-path crates/ps-decorate/Cargo.toml  # eyeball the 1a decorator on live ps output
cargo build --manifest-path src-tauri/Cargo.toml          # the Tauri app crate (needs GTK/webkit deps)

# Throughput benchmarks (Phase 5.1, §14) — headless, print MiB/s + fail on a lenient floor:
cargo run --release --manifest-path crates/shellint/Cargo.toml --example bench_scan   # OSC-scan MiB/s
cargo run --release --manifest-path crates/pty-core/Cargo.toml --example bench_pump   # PTY pump + flood
```

CI (`.github/workflows/ci.yml`) runs the headless crate tests, the frontend build, the
Tauri app build (installs GTK/webkit deps), and the two benchmarks — the bench numbers
are printed for trend tracking. GUI-dependent latency is a documented manual check (a
headless runner has no display).

The headless crates (`pty-core`, `config`) have unit tests and build without the GUI toolchain — use them as the fast inner loop. `npm run dev` (vite alone) has no Tauri IPC backend — always use `npm run tauri dev` to exercise the real app.

Building the Tauri app requires the Linux system deps in `README.md` (webkit2gtk, GTK dev libs, etc.).

**Packaging** lives in `packaging/` + `bundle.linux.{deb,rpm}` in `src-tauri/tauri.conf.json`. The **`.deb`** postinst registers Sampa as `x-terminal-emulator` via `update-alternatives` — a **Debian-only** mechanism. The **`.rpm`** scriptlets (`packaging/rpm/`) deliberately do **not** do this (there's no `/usr/bin/x-terminal-emulator` on Fedora); they only refresh the desktop database, and discovery is via the `TerminalEmulator` desktop-entry category. Don't copy the deb's alternatives call into the rpm. **Flatpak is declined for v1** (host-shell vs. sandbox — see `docs/adr/0001-flatpak-distribution.md`).

## Architecture

The core design principle (DESIGN.md §4) is a **renderer-agnostic core**: a headless, testable Rust core behind a thin frontend, so the webview renderer can be swapped for a native GPU renderer later *without rewriting the terminal*. Keep this seam clean — do not leak GUI/webview concerns into the core.

Four layers across two halves:

- **Layer 1 — PTY/process** (`crates/pty-core/`): pure-Rust, **zero GUI dependency** so it builds and tests standalone. Spawns the shell on a PTY via `portable-pty` (which handles the POSIX openpty/setsid/TIOCSCTTY dance, so job control and terminal signals work). Streams output bytes over an `mpsc::Sender` from a dedicated reader thread. This is the durable, long-lived asset. **Keep it free of any Tauri/webview imports.**
- **Config model** (`crates/config/`, `sampa-config`): another headless, GUI-free crate — serde/TOML `Config` with per-field defaults and XDG path resolution (DESIGN.md §11). Same rule as `pty-core`: no Tauri/webview imports.
- **CLI parser** (`crates/cli/`, `sampa-cli`): headless, std-only argv parser for the terminal CLI contract (`-e`, `--working-directory`, `--hold`, `--title`, …; DESIGN.md §12.2). Infallible (unknown flags → warnings, not a crash). CLI overrides apply to the *first* session only. Note the **ready-gate**: `spawn_session` parks a session's output pump until the frontend calls `session_ready`, so a fast `-e` command can't exit before listeners attach and lose its output.
- **Shell integration** (`crates/shellint/`, `sampa-shellint`): headless incremental `OscScanner` extracting OSC 7 (cwd) and OSC 133 (semantic prompt marks) from the PTY byte stream (DESIGN.md §5.6). The bridge runs it in each session's output pump, tracks per-session cwd (OSC 7 → else `/proc/<pid>/cwd`), serves `get_session_cwd`, and emits `shell://cwd|mark/<id>` events. Opt-in `shell-integration/sampa.{zsh,bash}` emit the marks.
- **Signature-feature services** (`crates/palette`, `crates/man`, `crates/preview`): headless cores for the M4 features — `$PATH` enumeration, `man -P cat` rendering (validated, no shell, nroff/ANSI-stripped), and the safe-preview **allowlist gate**. The man panel and preview detect the command from **tracked keystrokes** (`Tab.typed`, updated in `onData`), *not* by grid-scraping — reading the xterm buffer proved unreliable because p10k/autosuggestions redraw the prompt and desync the grid from what you'd read. When touching command detection, keep it keystroke-based. **The preview gate (`sampa_preview::classify`) is the security boundary (§13) — it is authoritative in the core and must never be weakened to depend on the frontend; a typed `rm` must stay filesystem-verified as never-run.**
- **`ps(1)` output enhancement** (`crates/ps-decorate`, `sampa-ps-decorate`): headless parse-and-decorate core for a **TTY-only presentation layer over unmodified `ps` output** (`docs/spec-ps-output-enhancement.md`). Recognises a `ps` table by its **exact** header signature (`header_kind`), parses rows, and produces a decorated model. It implements all three progressive levels behind `[enhance] ps` (`quiet | bars | inspector`, default `quiet`): **1a** quiet columns (zero elision, K/M/G size units, VSZ dropped, kernel threads folded to a count), **1b** signal bars (`bar`/`bars_for` — 8-cell block-glyph magnitude bars scaled to the column max + header denominators + live `c/m/p` sort), and **1c** the two-pane inspector (`classify`/`group_rows` provenance grouping with subtotals + a detail pane). **Every entry point fails safe to raw**: an unrecognised header or *any* malformed row returns `None` so the caller reprints the original bytes — a stream that merely resembles `ps` is never mangled (spec §3, mirroring the `preview` gate discipline). Pure, no Tauri imports (serde only, for the IPC model). The bridge command `decorate_ps(block, cols)` applies the config + width gate (`resolve_level`, spec §3) and returns the decorated model or `None`; the frontend decides *when* to offer a block (a manual `Ctrl+Shift+E` trigger over a scrollback scrape) and never sends piped/redirected output, so non-interactive `ps` stays byte-identical. The inspector detail pane is fed by `ps_enrich(pids)` — a **read-only** `ps -o …` query (no shell, numeric pids) parsed by the core's `parse_enrich`. The inspector's **`k`-to-signal** action keeps the **insert-never-run** boundary — it writes `kill <pid>` to the prompt, never executes. `START` locale-normalisation is still deferred (the raw field is carried through).
- **`cd` tree picker** (`crates/fsnav`, `sampa-fsnav`): headless read-only directory navigation for a `cd`-argument picker that **shares the `Ctrl+Shift+E` shortcut** — the frontend dispatches on the typed command (`cd` → tree; anything else → the ps decorator). `list_subdirs(path)` returns the immediate subdirectories (dirs only, symlinks followed, sorted, best-effort empty on error) so the frontend builds a **lazily-expanded** tree rooted at the session cwd; `relativize(root, child)` yields the compact argument. Pure `std::fs` (serde only), **no shell, no Tauri**, tested against temp-dir fixtures. The bridge command `list_dirs(path)` wraps it. Choosing a node **inserts `cd <path>` at the prompt (erasing the tracked line first), never executes** — the same insert-never-run boundary.
- **`du` disk-usage treemap** (`crates/dumap`, `sampa-dumap`): headless parse core for a squarified-treemap view that **also shares `Ctrl+Shift+E`** (typed `du` → treemap; the frontend dispatcher now routes `cd`→tree, `du`→treemap, else→ps). `parse_du(output)` turns `du -k` output (`<size>\t<path>`, cumulative KiB, post-order) into a nested `DuNode` tree with children sorted largest-first; fails safe to `None` on malformed input. Pure `std` + serde, **no shell, no Tauri**, tested against sample + real `du`. The bridge command `run_du(path)` runs a **read-only, timeout-bounded** `du -k -x --max-depth=4` off the async runtime (child killed on a 6s expiry, `du` can be slow) and parses it. The **squarified treemap layout lives in the frontend** (`squarify`, pixel-dependent) — the `#dumap` SVG overlay renders boxes sized by disk, click-to-zoom with a breadcrumb; **Enter inserts `cd <viewed dir>`, never executes**.
- **AI command suggester** (`crates/ai`, `sampa-ai`): headless NL→command service — one `POST /v1/messages` call (raw HTTPS via `ureq`; Rust has no Anthropic SDK) with a structured-output schema so `{command, explanation}` parses reliably. The request-build/response-parse core is pure and unit-tested against a fake `Transport`; the real `UreqTransport` is the **only code in the whole project that opens a socket**. This is a deliberate, bounded exception to the project's **zero-network-surface** rule (DESIGN.md §13 / cross-cutting security track), so it is **opt-in** (`[ai] enabled`, default `false`) and inert otherwise. The **API key is read from `ANTHROPIC_API_KEY` at runtime, never stored** in the config or the crate. The bridge command `suggest_command` runs the blocking call off the async runtime (`spawn_blocking`), gates on `enabled` + the env key, and only attaches terminal context when the user opted into `[ai] send_context`. The result is a **suggestion inserted at the prompt, never auto-run** — the same insert-never-run boundary as the palette. When touching this, keep it opt-in, keep the key out of config/logs/bodies, and never auto-execute the suggestion. `[ai] endpoint` is configurable so users can point at a local model and keep data on-device. The **inverse direction** is `sampa_ai::explain` + the `explain_command(command)` bridge command (`Ctrl+Shift+X`): send the **typed command line** to the API and show a plain-prose description in a read-only popup (`#explain`). Same opt-in gate + key-from-env + `spawn_blocking`; pressing the shortcut is the deliberate egress, and nothing is executed.
- **App shell** (`src-tauri/`): the only layer that knows about the webview. Bridges the headless crates to the frontend over Tauri IPC. Owns the `Sessions` table (from `pty-core`) and a `ConfigState`, plus a `notify` file-watcher that emits `config://changed` on config edits. Depends on `pty-core` and `sampa-config` by path.
- **Layers 3–4 — renderer + input** (`src/main.ts`, `index.html`): xterm.js in the Tauri webview. Deliberately thin — all terminal behavior belongs in the core. Fetches `get_config` on load and maps `Config` onto xterm options; re-applies on the `config://changed` event. Manages **tabs** (one xterm + `SearchAddon` + session per tab, over the shared `Sessions` table), a config-driven keybinding dispatcher (chords matched on physical `KeyboardEvent.code` so they're layout-independent), and the search overlay. UI concerns (tabs, search, keybinds, paste-confirm modal) live here, not in the core. **Escape-sequence hardening (§13) that acts on the webview lives here too:** the **OSC 52** handler (`term.parser.registerOscHandler(52, …)`) gates clipboard *writes* through `config.clipboard.osc52_write` (`ask` → consent modal, `allow`, `deny`) and **always denies reads** (`?`); `sanitizeTitle()` strips C0/DEL and caps length before any OSC 0/2 title reaches the DOM. When touching titles or clipboard, keep reads denied and route writes through the policy — don't call `navigator.clipboard` directly from a sequence handler. **Query-reply safety** lives here too: DA (`CSI c`), DSR (`CSI n`) and DECRQSS (`DCS $q`) are left to xterm, which answers with fixed / cursor-derived values only — leave them alone so apps keep working. The one echo vector is the **title report** (`CSI 20/21 t`): the title is application-settable, so reporting it back injects attacker bytes into stdin — it's pinned off in `windowOptions` (`getWinTitle`/`getIconTitle`) *and* swallowed by a defensive `registerCsiHandler({final:"t"})` that returns true only for ops 20/21. Don't enable those window-ops or widen that handler. **DECRQCRA** (`CSI …*y`, rectangular-area checksum) is implemented here too so the **esctest** conformance suite can read the screen back: it sums the requested cells' code points (empty cell = space `0x20`) and replies `DCS Pid !~ XXXX ST`. esctest reads one cell at a time, so the per-cell sum equals the char code; run esctest with `--xterm-checksum 334` (raw, non-negated convention). Geometry note: xterm.js intercepts `CSI 8t` (grid resize) internally and never hands it to a custom handler, so the app *cannot* letterbox a fixed grid — the suite runs at the window's natural size. Conformance runner + baseline live in `tools/conformance/` (gate on the pass count, baseline **305**).

### The IPC seam (DESIGN.md §9)

The frontend↔core protocol is intentionally small and symmetric (so a future native frontend uses the same *shape* as a function-call API):

- **Commands (frontend → backend):** `spawn_session(cols, rows) -> id`, `write_session(session, data)`, `resize_session(session, cols, rows)`.
- **Events (backend → frontend):** `pty://output/<id>` and `pty://exit/<id>`.

**Output is base64-encoded across the IPC boundary.** PTY output is raw bytes that may split mid-UTF-8 / mid-escape-sequence, so the backend base64-encodes each chunk and the frontend decodes to a `Uint8Array` and lets xterm.js reassemble. Do not treat PTY output as UTF-8 strings at the boundary.

### Conventions when extending

- New terminal behavior (VT handling, services like the man panel / palette / preview / config / sessions) goes in the **core**, not the frontend. The frontend only renders and captures input.
- New IPC calls follow the command/event split above and go through `src-tauri/src/lib.rs`'s `invoke_handler`.
- Section references in code comments (`DESIGN.md §5.1`, `§9`, etc.) point at the authoritative design doc — consult it before changing PTY lifecycle, resize/SIGWINCH, or the protocol.
