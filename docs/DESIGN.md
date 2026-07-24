# Graphical Terminal — Design & Implementation Guide

A complete, self-contained specification for building a **real graphical terminal
emulator for Linux**. This document is written to bootstrap a fresh repository; it
does not depend on any other project. It carries forward three differentiating
features prototyped earlier in the *Olinda* browser terminal — a **command
palette**, a **live manual panel**, and a **safe auto-run preview** — and folds
them into a proper native terminal design.

> Status: design blueprint. Nothing here is prescriptive law — pick the recommended
> path or diverge with eyes open. Sections marked **[Decision]** are the ones worth
> settling before writing code.

---

## Table of contents

1. [Vision, scope, goals & non-goals](#1-vision-scope-goals--non-goals)
2. [Glossary](#2-glossary)
3. [What makes a terminal "real": the contract](#3-what-makes-a-terminal-real-the-contract)
4. [Architecture](#4-architecture)
5. [Layer 1 — the PTY / process layer](#5-layer-1--the-pty--process-layer)
6. [Layer 2 — the VT emulation layer](#6-layer-2--the-vt-emulation-layer)
7. [Layer 3 — the renderer](#7-layer-3--the-renderer)
8. [Layer 4 — input](#8-layer-4--input)
9. [The IPC / core-to-frontend protocol](#9-the-ipc--core-to-frontend-protocol)
10. [Signature features (palette, man, preview)](#10-signature-features-palette-man-preview)
11. [Configuration model](#11-configuration-model)
12. [Linux desktop integration](#12-linux-desktop-integration)
13. [Security model](#13-security-model)
14. [Performance](#14-performance)
15. [Repository layout](#15-repository-layout)
16. [Build, packaging, distribution](#16-build-packaging-distribution)
17. [Testing & conformance](#17-testing--conformance)
18. [Milestones / roadmap](#18-milestones--roadmap)
19. [Risks & open questions](#19-risks--open-questions)
20. [References](#20-references)

---

## 1. Vision, scope, goals & non-goals

**Vision.** A fast, modern, GPU-accelerated terminal emulator for Linux (Wayland +
X11) that runs your login shell as a first-class desktop application, can be set as
the system default terminal, and layers on three productivity features most
terminals lack: an in-app command palette, a live man-page panel, and a safe
"preview as you type" pane.

**In scope**
- A graphical (windowed) terminal emulator that spawns a real shell over a PTY.
- Faithful `xterm-256color`-class VT emulation (vim / tmux / htop / less / neovim /
  emacs -nw / weechat / ssh all work).
- Truecolor, mouse reporting, bracketed paste, alt-screen, scrollback, selection,
  clipboard, hyperlinks (OSC 8), and optionally inline images (sixel / kitty
  graphics — honoring the "graphical" in the name).
- Tabs and (optionally) splits.
- The three signature features.
- Linux packaging and default-terminal registration.

**Out of scope (non-goals)**
- Running on the bare Linux text console (tty1). This is a *graphical* emulator; it
  needs a display server. "Terminal in the console" is a different category.
- Windows / macOS as first-class targets (the design stays portable where cheap, but
  Linux is the target).
- A multiplexer (tmux-like) — integrate with tmux instead of replacing it.
- A shell. We run the user's `$SHELL` (default zsh); we do not implement one.

**Quality bar for "v1"**
- Passes the everyday app matrix (§17) with no visible corruption.
- ≥95% pass on the relevant subset of `esctest`.
- Sub-frame added input latency on a 60 Hz display (measured with typometer-style
  tooling).
- Can `cat` a multi-megabyte file without stalling the UI thread.

---

## 2. Glossary

| Term | Meaning |
|---|---|
| **PTY** | Pseudoterminal: a kernel device pair (master/slave) that emulates a serial terminal. The emulator holds the master; the shell runs on the slave. |
| **Line discipline** | Kernel layer on the slave that does cooked-mode editing, echo, and signal generation (^C→SIGINT). |
| **Controlling terminal** | The terminal a session is attached to; enables job control and terminal signals. |
| **VT / ANSI sequences** | Escape/control byte sequences (ECMA-48 + DEC private) that move the cursor, set colors, switch screens, etc. |
| **CSI / OSC / DCS** | Control Sequence Introducer (`ESC [`), Operating System Command (`ESC ]`), Device Control String (`ESC P`). |
| **SGR** | Select Graphic Rendition — the CSI that sets color/bold/underline etc. |
| **Alt screen** | Secondary full-screen buffer (mode 1049) used by full-screen apps like vim. |
| **terminfo** | Database describing a terminal's capabilities, keyed by `$TERM`. |
| **SIGWINCH** | Signal delivered to the foreground process group when the window size changes. |
| **Grid / cell** | The screen model: a 2-D array of cells, each holding a grapheme + attributes. |
| **Damage** | The set of cells changed since the last frame; used to avoid full redraws. |

---

## 3. What makes a terminal "real": the contract

A "real terminal" is not about rendering — it is about honoring the contracts that
command-line programs expect. Get these right and vim/tmux/ssh "just work."

1. **A PTY with a controlling terminal.** The child must `setsid()` then acquire the
   slave as its controlling terminal (`ioctl(TIOCSCTTY)`). This is what makes job
   control (Ctrl-Z, `fg`/`bg`), `tty(1)`, and terminal signals work.
2. **Correct signal behavior.** The emulator only ever writes *bytes* to the master;
   the kernel line discipline turns `^C`/`^Z`/`^\` into SIGINT/SIGTSTP/SIGQUIT for
   the foreground process group. Do not synthesize signals yourself.
3. **Window size + SIGWINCH.** On resize, set the slave `winsize` (`ws_row`,
   `ws_col`, and `ws_xpixel`/`ws_ypixel` — pixels matter for image protocols) via
   `TIOCSWINSZ`; the kernel delivers SIGWINCH automatically.
4. **`$TERM` + terminfo agreement.** Advertise `TERM=xterm-256color` (universal,
   safe) and implement what that terminfo entry promises. Set `COLORTERM=truecolor`
   so apps enable 24-bit color. (Optional later: ship a custom terminfo compiled
   with `tic` describing exactly your capabilities.)
5. **VT sequence coverage.** Cursor movement, SGR (incl. 256/truecolor), scroll
   regions (DECSTBM), origin mode, insert/replace, tab stops, character sets,
   alt-screen (1049), mouse (1000/1002/1003 + SGR 1006), bracketed paste (2004),
   focus events (1004), title stack (OSC 0/1/2), OSC 4/10/11 color queries,
   OSC 8 hyperlinks, OSC 52 clipboard.
6. **Keyboard encoding.** Map key + modifiers to the correct byte sequences,
   respecting application cursor-key mode (DECCKM) and keypad mode, plus at least
   one modern modifier-encoding scheme (`modifyOtherKeys` / CSI-u / the kitty
   keyboard protocol).
7. **A clean exit.** Reap the child (`SIGCHLD`/`waitpid`), surface its exit status,
   and tear the window/tab down.

Everything else (fonts, GPU, tabs, themes) is polish on top of this contract.

---

## 4. Architecture

### 4.1 Layered model (renderer-agnostic core)

Design the system as four layers with a clean seam between the **core** (headless,
testable) and the **frontend** (rendering + input). This is the single most
important architectural decision: it lets you swap the renderer (webview today,
native GPU later) without rewriting the terminal.

```
┌──────────────────────────────────────────────────────────────┐
│ FRONTEND (per window/tab)                                      │
│  Layer 4  Input       key/mouse → byte sequences               │
│  Layer 3  Renderer    grid → pixels (webview xterm.js OR GPU)  │
├───────────────────────  IPC / in-proc API  ───────────────────┤
│ CORE (headless, unit-testable)                                 │
│  Layer 2  VT emulation  bytes → grid + events                  │
│  Layer 1  PTY/process   spawn shell, r/w master, resize, reap  │
│  Services  palette · man · preview · config · sessions         │
└──────────────────────────────────────────────────────────────┘
```

The seam is a small message protocol (§9). In a single-process native app it is a
function-call API; in a webview app it is IPC. Keep the *shape* identical so the
core never knows which frontend it has.

### 4.2 Stack options **[Decision]**

| | **A. Electron** | **B. Tauri / wry (recommended)** | **C. Fully native** |
|---|---|---|---|
| Core language | Node/TS | **Rust** | Rust (or C++/Zig) |
| Emulation | xterm.js | **xterm.js** (webview) | `alacritty_terminal` / `vte` / libvterm |
| Renderer | Chromium + WebGL addon | **webkitgtk + WebGL addon** | wgpu / OpenGL, custom glyph atlas |
| PTY | node-pty | **portable-pty** (Rust) | portable-pty / `nix::pty` |
| Binary size | ~120 MB+ | **~10–20 MB** (system webkit) | **~5 MB** |
| Effort to MVP | Lowest | **Low–Medium** | High |
| Perf ceiling | Good | Good | **Best** |
| Precedent | Hyper, VS Code | Rio (partly), Warp-ish | Alacritty, kitty, foot, wezterm |

**Recommendation:** start on **Path B (Tauri + xterm.js)**. Rationale:
- A **Rust core** (PTY, parser bridge, services) is the long-lived, valuable asset;
  it survives even if you later replace the webview with a native renderer.
- xterm.js is a battle-tested emulator (ships in VS Code); you get correct VT
  behavior on day one instead of spending months on conformance.
- The system webview keeps the binary small and gives you fonts, clipboard, IME,
  and OSC-8 links "for free."
- The layered seam means **Path C is a later renderer swap, not a rewrite** — you
  can migrate Layer 3 to `alacritty_terminal` + wgpu when/if perf demands it, reusing
  Layers 1–2 logic and all services.

Choose **A (Electron)** only if the team is JS-first and wants the absolute fastest
start. Choose **C** from the outset only if native performance is a hard requirement
and you're prepared to own VT conformance yourself.

The rest of this guide is written against **Path B**, calling out where Path C
differs.

### 4.3 Process & threading model

- **One core per window** is simplest; **one core process hosting many
  tabs/sessions** is more efficient. Recommended: a single core with a **session
  table** (`session_id → {pty, parser, grid}`), each session driven on its own async
  task.
- **Never block the UI thread on PTY I/O.** Read the master in a dedicated
  async task, feed bytes into the parser, and emit *coalesced* grid updates to the
  frontend (one update per display frame, not per byte).
- Backpressure: if a program floods output (`yes`, `cat huge`), parse eagerly but
  render at most once per vsync; drop intermediate frames, never intermediate state.

---

## 5. Layer 1 — the PTY / process layer

Responsibilities: open a PTY, spawn `$SHELL` on the slave with a controlling
terminal, pump bytes both ways, handle resize, and reap the child.

### 5.1 Opening the PTY (POSIX flow)

```
posix_openpt(O_RDWR|O_NOCTTY) -> master fd
grantpt(master); unlockpt(master)
slave_name = ptsname(master)
fork():
  child:
    setsid()                       # new session, detach old ctty
    open(slave_name, O_RDWR)       # becomes controlling terminal…
    ioctl(fd, TIOCSCTTY, 0)        # …made explicit
    dup2(slave, 0/1/2); close extras
    tcsetattr(...)                 # sane termios (usually inherited defaults)
    execvp(shell, argv)            # e.g. ["-zsh"] for a login shell
  parent:
    close(slave); keep master
```

**Use a library.** In Rust, `portable-pty` (from the wezterm project) does all of
the above cross-platform, including `TIOCSCTTY`. In Node, `node-pty`. Do not
hand-roll unless you must.

### 5.2 Spawn parameters

- **Command:** `$SHELL` (fallback `/bin/sh`). Login shell mode → argv[0] prefixed
  with `-` (e.g. `-zsh`) *or* pass `-l`. Make this a config toggle.
- **Working directory:** inherit the launching cwd; honor `--working-directory`.
- **Environment:** inherit, then set:
  - `TERM=xterm-256color`
  - `COLORTERM=truecolor`
  - `TERM_PROGRAM=<yourname>`, `TERM_PROGRAM_VERSION=<v>`
  - unset `COLUMNS`/`LINES` (let the PTY drive size)
  - a session-unique var (e.g. `<NAME>_SESSION_ID`) for the palette/preview services
- **Initial size:** compute rows/cols from the window pixel size ÷ cell metrics
  *before* spawn so the shell's first prompt is correct.

### 5.3 Resize

On window/font change: recompute `cols = floor(width_px / cell_w)`,
`rows = floor(height_px / cell_h)`; call `pty.resize(rows, cols, xpixel, ypixel)`
(→ `TIOCSWINSZ` → SIGWINCH). Debounce during live drags. Include pixel dimensions
so image protocols compute geometry correctly.

### 5.4 Lifecycle

- Read loop: `read(master)` → bytes → parser. On EOF/`EIO`, the child exited.
- Reap via `SIGCHLD`/`waitpid`; capture exit code + signal.
- On shell exit: emit a `session-exit` event; per config, close the tab, or keep it
  open showing `[process exited: N] — press Enter to close`.
- On window close: `SIGHUP` the child, close master.

### 5.5 Knowing the child's cwd (needed by the signature features)

On Linux, read `readlink(/proc/<child_pid>/cwd)`. The `child_pid` is the shell's
pid from spawn. Track the **foreground** process too (see §5.6) if you want the cwd
of the running program rather than the shell.

### 5.6 Foreground process / shell integration (optional but powerful)

- `tcgetpgrp(master)` gives the foreground process group id; combined with `/proc`
  you can show the running command in the tab title, decide when the prompt is idle,
  etc.
- Even better: **shell integration** via OSC 133 (semantic prompt marks) and OSC 7
  (cwd reporting). Ship opt-in shell snippets (zsh/bash/fish) that emit:
  - `OSC 7 ; file://host/path` on `chpwd` → authoritative cwd (better than `/proc`).
  - `OSC 133 ; A/B/C/D` around prompt / command / output → enables "jump to prompt,"
    command status, and reliable "is the user at an idle prompt?" detection that the
    signature features want.

---

## 6. Layer 2 — the VT emulation layer

Turns the master byte stream into a **grid model** + a stream of **events**
(title changed, bell, clipboard request, resize request, cwd changed…).

### 6.1 Build vs. reuse **[Decision]**

- **Path B:** reuse **xterm.js** in the webview. It already implements the parser,
  grid, modes, mouse, bracketed paste, alt-screen, and exposes `onData`,
  `onTitleChange`, `onResize`, `buffer`, addons (WebGL, image, web-links, search,
  fit, unicode11). *Do not reimplement this.*
- **Path C:** use a Rust crate — `alacritty_terminal` (full grid + parser, the
  engine behind Alacritty) or `vte` (parser only) + your own grid, or C `libvterm`.
  Reusing `alacritty_terminal` is strongly preferred over writing a parser.

If you ever *do* write the parser: implement Paul Williams' DEC ANSI state machine
(the canonical reference, vt100.net). Do not parse with regex/ad-hoc string
matching — it is a genuine state machine (GROUND, ESCAPE, CSI_ENTRY, CSI_PARAM,
OSC_STRING, DCS_*, …).

### 6.2 The grid model

- Cell: `{ grapheme, fg, bg, flags(bold/italic/underline{styles}/strikethrough/
  inverse/invisible/dim), hyperlink_id, wide? }`.
- Screen: primary buffer + alt buffer; cursor (pos, style, visibility); saved cursor
  (DECSC/DECRC); scroll region (top/bottom); tab stops; charset slots (G0–G3);
  modes (DECAWM autowrap, DECOM origin, IRM insert, LNM, reverse-video…).
- **Scrollback:** a ring buffer of rows with a configurable cap (e.g. 10k). Only the
  primary buffer scrolls back; alt screen never does.
- **Unicode:** grapheme clustering + correct wide-char (East-Asian width) handling;
  combining marks attach to the base cell. (xterm.js: enable the unicode11 addon.)

### 6.3 Events the core must surface

`title`, `icon-title`, `bell`, `clipboard-write (OSC 52)`, `clipboard-read`,
`hyperlink`, `cwd (OSC 7)`, `prompt-mark (OSC 133)`, `color-query (OSC 4/10/11)`,
`resize-request (DECCOLM etc.)`, `mouse-mode-change`, `bracketed-paste-change`,
`image (sixel/kitty)`.

### 6.4 Images (optional, honors "graphical")

- **Sixel:** xterm.js `@xterm/addon-image`, or decode in the core.
- **Kitty graphics protocol** (`APC G ... ST`) and/or **iTerm2 inline images**
  (`OSC 1337`): higher fidelity, needs explicit support.
- Native path: composite decoded images into the GPU scene at cell coordinates,
  clipped by scroll region; free them on scroll-out. **Cap size and count** (a
  security/DoS concern — §13).

---

## 7. Layer 3 — the renderer

### 7.1 Path B (webview)

- xterm.js DOM renderer for MVP; switch on **`@xterm/addon-webgl`** for throughput.
- `@xterm/addon-fit` for sizing, `@xterm/addon-web-links` or a custom OSC-8 handler
  for links, `@xterm/addon-search`, `@xterm/addon-unicode11`.
- The webview gives you font loading + fallback, subpixel/AA, ligatures (if the font
  has them and you allow it), clipboard, and IME.

### 7.2 Path C (native) — what you'd own

- **Glyph rasterization + shaping:** `cosmic-text` (layout + shaping + fontdb +
  swash rasterizer) is the highest-leverage choice; alternatives: harfbuzz +
  freetype, or `swash`/`fontdue` directly. Handle **font fallback** (fontconfig) for
  missing glyphs and **ligatures** (opt-in; disable inside code by default is a
  common choice).
- **GPU:** `wgpu` (or GL). Maintain a **glyph atlas** (texture cache keyed by
  glyph+size+subpixel); draw cells as textured quads with per-cell fg/bg. Background
  fills as a separate instanced pass.
- **Damage tracking:** redraw only changed cells; full redraw on scroll/resize.
- **Cursor** styles (block/bar/underline, blink), **selection** highlight,
  **search** highlight, and **dim inactive** are all renderer concerns.
- Windowing via `winit` (X11 + Wayland), or a toolkit (GTK4) if you want native
  chrome.

### 7.3 Shared renderer concerns (both paths)

- Theme: 16 ANSI colors + default fg/bg/cursor/selection; truecolor passthrough;
  support OSC 4/10/11 dynamic color set/query.
- Fonts: family + size + fallback chain; line height; letter spacing; box-drawing
  and Powerline glyph alignment.
- Ligatures: off inside terminals is safest; make it a toggle.
- Bell: visual flash and/or audible; per config.

---

## 8. Layer 4 — input

The fiddliest correctness area after the parser. On Path B, xterm.js handles most of
this; on Path C you own it.

### 8.1 Keyboard → bytes

- Printable keys → UTF-8 bytes (respect IME/dead keys/compose).
- Control keys → C0 (`Ctrl-A`→0x01 … `Ctrl-C`→0x03, `Ctrl-[`→ESC, etc.).
- Special keys (arrows, F-keys, Home/End/PgUp, keypad) → CSI/SS3 sequences, and they
  **change with modes**: application cursor keys (DECCKM) and keypad mode alter the
  bytes.
- Modifiers on special keys → xterm `modifyOtherKeys`, **CSI-u (fixterms)**, or the
  **kitty keyboard protocol** (progressive enhancement, `CSI > flags u`). Support at
  least CSI-u; kitty protocol is a nice-to-have that unlocks apps like neovim's
  richer mappings.
- **Bracketed paste:** when mode 2004 is on, wrap pasted text in `ESC[200~ … ESC[201~`
  so apps don't execute it as keystrokes.

### 8.2 Mouse

When a mouse mode is enabled (1000 click, 1002 drag, 1003 any-motion) encode events;
prefer **SGR encoding (1006)** (`CSI < b ; x ; y M/m`) — it has no 223-column limit.
When mouse mode is *off*, the mouse drives local selection instead.

### 8.3 Clipboard & selection

- Selection: click-drag (char), double (word), triple (line); block selection with a
  modifier. Auto-copy-on-select is a config option (X11 primary selection idiom).
- Copy: `Ctrl-Shift-C`; paste: `Ctrl-Shift-V` (and middle-click → primary on X11).
- Paste safety: strip/deny embedded `ESC[201~`; consider **confirming multi-line
  pastes** (paste-injection mitigation, §13).
- Honor **OSC 52** for programmatic clipboard — but gate writes (§13).

### 8.4 Keybindings

A configurable table mapping chords → actions (copy, paste, new tab, split, search,
zoom, open palette, toggle man, toggle preview…). Chords that aren't bound fall
through to the terminal as byte sequences. Reserve a namespace (e.g. `Ctrl-Shift-*`)
for app actions so shell keys aren't shadowed.

---

## 9. The IPC / core-to-frontend protocol

Keep the seam small and identical across native/webview. Suggested messages:

**Frontend → Core**
```jsonc
{ "t": "input",  "session": 1, "data": "ls\r" }          // raw bytes to PTY
{ "t": "resize", "session": 1, "cols": 120, "rows": 34, "xpixel": 1440, "ypixel": 748 }
{ "t": "spawn",  "cwd": "/home/u", "shell": null, "login": true }   // -> session id
{ "t": "close",  "session": 1 }
{ "t": "palette-list" }                                   // -> commands
{ "t": "man",     "session": 1, "cmd": "grep" }
{ "t": "preview", "session": 1, "cmd": "ls -l" }
{ "t": "clipboard-answer", "session": 1, "granted": true } // OSC 52 gate
```

**Core → Frontend**
```jsonc
{ "t": "ready",   "session": 1, "shell": "/usr/bin/zsh", "pid": 4321 }
{ "t": "output",  "session": 1, "bytes": "…" }            // raw PTY bytes (or pre-parsed grid deltas)
{ "t": "title",   "session": 1, "title": "vim README" }
{ "t": "cwd",     "session": 1, "path": "/home/u/src" }   // from OSC 7 or /proc
{ "t": "bell",    "session": 1 }
{ "t": "exit",    "session": 1, "code": 0, "signal": null }
{ "t": "clipboard-write", "session": 1, "text": "…", "needsConfirm": true } // OSC 52
```

Two viable division-of-labor choices **[Decision]**:
- **Bytes over the seam** (Path B with xterm.js): core forwards raw PTY bytes;
  xterm.js parses in the frontend. Simplest; xterm.js owns Layer 2.
- **Grid deltas over the seam** (Path C, or a headless core): core parses and sends
  cell diffs; frontend only draws. More work but renderer-agnostic and testable.

For Path B, ship **bytes**; migrate to **grid deltas** if/when you go native.

---

## 10. Signature features (palette, man, preview)

These are the product differentiators. In a single-machine native app they become
**in-process core services** — no network, no server, no open port. Each needs the
session's shell pid (for cwd) which the core already has.

### 10.1 Command palette

- **Data:** enumerate executables on `$PATH` (scan each dir, keep entries with an
  execute bit; dedupe; sort). Cache with a short TTL; invalidate on `$PATH` change.
- **UX:** a hotkey (reserve `Ctrl-Shift-P`) opens a filtered dropdown; type to fuzz,
  arrows to move, Enter to **insert** the command at the prompt (write
  `"<cmd> "` to the PTY — do **not** auto-run). Escape closes.
- Native path: render the palette as an overlay in the renderer; webview path: a DOM
  overlay above the terminal surface.

### 10.2 Live manual panel

- As the user types, detect the command at the prompt and render `man <cmd>` in a
  side panel.
- **Command detection:** read the grid's cursor row, walk **up** to the prompt line
  (so multi-line constructs resolve to their leading keyword), and take the first
  token **up to the cursor column** (so shell autosuggestions drawn past the cursor
  aren't mistaken for typed input). Debounce (~300 ms). Gate on the token being a
  real `$PATH` command; **collapse the panel** when it isn't (a keyword like `for`)
  or when `man` has no entry — never leave a stale page showing.
- **Rendering `man`:** run `man -P cat <cmd>` in a subprocess (arg vector, **no
  shell**; validate `cmd` against `^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$`); strip nroff
  overstrike (`X\bX`) and ANSI SGR; cache per command.
- **Note on prompt-reading fragility:** grid-scraping the prompt is heuristic.
  If you adopt **OSC 133 shell integration** (§5.6) you get exact prompt/command
  boundaries and this becomes robust instead of best-effort. Strongly recommended.

### 10.3 Safe auto-run preview

As the user types a **syntactically valid, read-only** command, run it in a
throwaway shell (in the session's real cwd) and show the output in a panel —
without touching the interactive session. This is the highest-risk feature; the
design is safety-first.

- **Detect the typed line** (same cursor-column technique as the man panel, so
  autosuggestions are excluded). Debounce ~550 ms.
- **Gate, server-side / in-core and authoritative:**
  1. **Syntax check** with `zsh -n -c <line>` (parses, never executes) → incomplete
     lines like `ls |` are "incomplete," not run.
  2. **Safety classify:** reject shell metacharacters that enable writes/side
     effects/hidden execution (`; & < > \` $() && || >>`), env assignments, and any
     command not on a **read-only allowlist**; allow a bare command or a `|`
     pipeline of allow-listed commands. `git` only for read-only subcommands;
     `find` rejected if it has `-exec/-delete/...`; `tail -f` rejected.
  3. **Execute** the survivor via `execFile(zsh, ["-c", line])` with a **timeout**,
     **output cap**, `SIGKILL` on timeout, **stdin closed** (so `cat`/`sort` with no
     args get EOF instead of hanging), and **cwd = the session's dir**.
- **Clear the preview on Enter** (watch for `\r` in the input stream): once the line
  is submitted, the real output is in the terminal and the preview is stale.
- **Honesty about limits:** "read-only" is a conservative allowlist, not a proof; a
  safe command can still read sensitive files (it's the user's own machine). Keep the
  allowlist small and documented. Consider a global on/off toggle, default on.

### 10.4 Why these fit better natively than in a browser

The browser prototype needed a WebSocket + HTTP endpoints and an unauthenticated
shell on a loopback port. Natively, all three features are in-process function calls
with **zero network surface** — the single biggest security improvement of going
native.

---

## 11. Configuration model

- **Format:** TOML (Alacritty/wezterm idiom) or YAML. Live-reload on change.
- **Location:** `$XDG_CONFIG_HOME/<name>/config.toml` (fallback `~/.config/...`).
- **Sections:** `font` (family, size, fallback, ligatures), `colors` (theme + 16
  ANSI + fg/bg/cursor/selection), `window` (padding, opacity, decorations,
  startup rows/cols), `scrollback` (lines), `shell` (program, args, login), `cursor`
  (shape, blink), `keybindings` (chord → action table), `bell`, and feature toggles
  (`palette`, `man`, `preview`, plus preview allowlist overrides).
- Ship a documented default config and a `--config` override flag.

---

## 12. Linux desktop integration

This is what turns "an app that runs a shell" into "my terminal."

### 12.1 Desktop entry

`/usr/share/applications/<name>.desktop`:
```ini
[Desktop Entry]
Type=Application
Name=<Your Terminal>
Comment=A graphical terminal emulator
Exec=<name> %F
Icon=<name>
Terminal=false
Categories=System;TerminalEmulator;
Keywords=shell;prompt;command;cmd;
StartupNotify=true
```

### 12.2 The command-line contract

Support the conventions launchers and apps rely on:
- `<name>` — open with the default shell.
- `<name> -e CMD [ARGS...]` and `<name> -- CMD [ARGS...]` — run CMD instead of the
  shell (historical `-e` is widely used by `x-terminal-emulator -e ...`).
- `<name> --working-directory=DIR`, `--title=STR`, `--hold` (keep open after exit),
  `--class=STR` (X11 WM_CLASS), `--config=FILE`.
- Respect `$SHELL`; login-shell toggle.

### 12.3 Becoming the default terminal

There is **no single mechanism** across desktops — implement several:
- **Debian/Ubuntu alternatives:**
  `update-alternatives --install /usr/bin/x-terminal-emulator x-terminal-emulator /usr/bin/<name> 50`
  (and a matching `.1.gz` manpage alternative). Many apps call
  `x-terminal-emulator -e`.
- **freedesktop emerging spec:** implement/participate in **`xdg-terminal-exec`**
  (the emerging standard for "the user's terminal"); provide the entry so
  spec-aware launchers pick you.
- **GNOME:** newer GNOME removed the settings key; document that GNOME users may need
  the `xdg-terminal-exec` path or per-app settings. Don't promise a one-click
  default on GNOME.
- **XFCE/KDE/others:** each has its own "preferred applications" UI pointing at your
  `.desktop`; ensure the `TerminalEmulator` category is set.

### 12.4 Windowing, clipboard, IME

- **Wayland + X11:** the webview/toolkit handles both (Path B). Native (Path C):
  `winit` covers both; test both — Wayland has no global window positioning, X11 has
  primary selection.
- **Clipboard:** X11 has CLIPBOARD + PRIMARY (middle-click paste); Wayland has
  `wl_data_device` + primary-selection protocol. The webview handles this; native
  code uses `wl-clipboard`/`arboard`/toolkit APIs.
- **IME / CJK / dead keys / compose:** free in the webview; native needs IME
  integration (ibus/fcitx via GTK, or `winit`'s IME events) — budget for it.

---

## 13. Security model

A terminal renders **untrusted bytes from arbitrary programs**. Treat output as
hostile.

- **Escape-sequence hardening:** never let output run commands. Notable vectors:
  - **OSC 52 clipboard *write*** — an app can silently set your clipboard. Gate it:
    prompt or restrict to focused/interactive, and disable clipboard *reads* by
    default.
  - **OSC 8 hyperlinks** — never auto-open; require an explicit click, and show the
    target.
  - **Title (OSC 0/2) injection** — sanitize control chars; don't reflect titles
    into shells or logs unescaped.
  - **Images (sixel/kitty)** — cap dimensions, total memory, and count; a malicious
    stream can OOM you.
  - **DECRQSS / color queries / DA responses** — reply with fixed, safe answers; do
    not echo attacker-controlled data back onto the input stream unfiltered.
- **Paste-injection:** enable **bracketed paste**; consider confirming pastes that
  contain newlines (a pasted `\n` runs a command).
- **Auto-run preview:** the allowlist gate (§10.3) is the security boundary and is
  **authoritative in the core**; the frontend can never bypass it. Verify with a
  test that a typed `rm <file>` leaves the file intact.
- **No network surface:** the native app opens no ports (unlike the browser
  prototype). Keep it that way — features are in-process.
- **Privilege:** runs as the user, no elevation. Don't add setuid anything.

---

## 14. Performance

- **Off-thread PTY read**, batched parse, **one render per vsync**. Coalesce bursts;
  never render per-byte.
- **Damage-based drawing** (redraw changed cells only); full redraw only on
  scroll/resize/theme change.
- **GPU glyph atlas** (native) or **WebGL addon** (webview) for throughput.
- **Scrollback** as a capped ring buffer; avoid reallocating rows.
- **Reflow on resize** is expensive — do it incrementally; it's fine to be O(n) in
  scrollback but keep constants low.
- **Benchmarks to track:** `time cat 50MB.log` (throughput), typometer-style **added
  input latency** (target < one frame), `yes | head -c 100M` (flood stability),
  memory with 100k-line scrollback.
- Latency tricks: render on input immediately (don't wait for the echo round-trip to
  *display the cursor*), and prefer immediate presentation modes.

---

## 15. Repository layout

Reference layout for **Path B (Tauri)**:

```
<name>/
├─ Cargo.toml                 # Rust workspace
├─ src-core/                  # Layer 1–2 + services (Rust, headless, unit-tested)
│  ├─ pty.rs                  # portable-pty spawn/resize/reap, /proc cwd
│  ├─ session.rs              # session table
│  ├─ services/
│  │  ├─ palette.rs           # PATH scan + cache
│  │  ├─ man.rs               # man -P cat, sanitize, cache
│  │  └─ preview.rs           # syntax check + allowlist gate + sandboxed run
│  └─ protocol.rs             # message enums (shared shape with frontend)
├─ src-tauri/                 # Tauri glue: commands/events <-> src-core
├─ frontend/                  # Layer 3–4 (TS)
│  ├─ terminal.ts             # xterm.js setup, addons, IPC wiring
│  ├─ input.ts                # keybindings, paste safety
│  ├─ palette.ts  man.ts  preview.ts   # the three panels
│  └─ styles.css
├─ shared/protocol.ts         # TS mirror of protocol.rs
├─ config/                    # default config.toml, themes
├─ packaging/                 # .desktop, icons, AppImage/deb/flatpak manifests
├─ docs/                      # this guide + ADRs
└─ tests/                     # parser fixtures, esctest harness, app-matrix scripts
```

For **Path C**, replace `frontend/` + `src-tauri/` with a native `src-render/`
(wgpu + cosmic-text + winit) and swap xterm.js for `alacritty_terminal` in
`src-core/`.

---

## 16. Build, packaging, distribution

- **Build:** `cargo` for the core; Tauri CLI bundles the app (Path B). Electron
  Forge/Builder for Path A.
- **Packages:** ship several — **AppImage** (portable), **.deb/.rpm** (native
  installs, can register `x-terminal-emulator`), **Flatpak** (sandboxed; note
  Flatpak restricts PTY/host access — you'll need `--talk-name`/`--filesystem=host`
  and it complicates the "run the host shell" story; document the trade-off), and a
  **Nix** flake if you like.
- **terminfo:** rely on the system's `xterm-256color`. If you ship a custom entry,
  compile with `tic` and install it, and gracefully fall back to `xterm-256color`
  when yours isn't present (e.g. over ssh to a host that lacks it).
- **Icons + .desktop** installed to the XDG paths; run `update-desktop-database`.
- **CI:** build matrix for X11 + Wayland; run the parser tests and a headless subset
  of the app matrix.

---

## 17. Testing & conformance

- **Parser unit tests:** byte-sequence fixtures → expected grid state (golden
  snapshots). The most valuable tests you'll write.
- **`esctest`** (xterm's automated conformance suite): run it, track pass rate,
  gate releases on a threshold for the sequences you claim to support.
- **`vttest`** (interactive): manual smoke of cursor movement, attributes, scrolling,
  character sets, mouse.
- **Real-app matrix** (the truth test): `vim`, `neovim`, `tmux`, `htop`, `less`,
  `mc`, `git log`, `ssh`, `weechat`, `emacs -nw`, `python`/`ipython`, `fzf`,
  a truecolor test script, a sixel image, a Unicode/CJK/emoji width test. Each must
  render without corruption and respond to resize.
- **Signature-feature tests:** palette inserts (not runs); man panel opens for real
  commands and **closes** for keywords/no-man; preview runs read-only, **refuses**
  writes (filesystem-verified `rm` leaves file intact), and **clears on Enter**.
- **Perf regression:** throughput + latency numbers in CI trend.

---

## 18. Milestones / roadmap

- **M0 — Echo (proof of life):** window + PTY + xterm.js (or native grid); type
  `ls`, see output; resize works. *Validates Layers 1–3 and the seam.*
- **M1 — A usable terminal:** full keyboard/mouse encoding, scrollback, selection,
  clipboard, bracketed paste, alt-screen apps (vim/htop/tmux green), signals/job
  control, clean exit. *This is the "real terminal" contract (§3).*
- **M2 — Comfort:** config file + live reload, themes, fonts/fallback, cursor styles,
  tabs (and optionally splits), search.
- **M3 — Linux citizen:** `.desktop`, `-e`/`--`/`--working-directory`, WM_CLASS,
  `x-terminal-emulator` + `xdg-terminal-exec`, packaging (AppImage + deb).
- **M4 — Signature features:** palette, man panel, safe preview (+ optional OSC 7/133
  shell integration to make them robust).
- **M5 — Polish & scale:** WebGL/GPU renderer, images (sixel/kitty), hyperlinks,
  ligatures toggle, perf pass, conformance (esctest threshold), Flatpak/rpm.
- **v1:** app matrix green, esctest threshold met, latency/throughput targets met,
  docs + config reference complete.

A realistic path: M0 in days, M1 in a few weeks (mostly input-encoding correctness),
M2–M3 a few weeks each, M4 building directly on the earlier prototype logic, M5
open-ended.

---

## 19. Risks & open questions

- **Keyboard encoding is the sink for correctness bugs.** Budget real time for
  modifier encodings and app cursor/keypad modes; lean on xterm.js early to sidestep
  it, or on `alacritty_terminal`'s reference behavior.
- **Prompt-scraping fragility** for the man/preview features. **Mitigation:** adopt
  OSC 133/7 shell integration and treat grid-scraping as the fallback.
- **webkitgtk quirks** (Path B): rendering perf and occasional canvas/WebGL issues on
  some distros. Evaluate early on your target distros; Electron is the escape hatch.
- **Flatpak vs. "run the host shell":** sandboxing fights a terminal's purpose.
  Decide the distribution story early **[Decision]**.
- **GNOME default-terminal story** is genuinely messy post-key-removal; set user
  expectations in docs.
- **Native renderer scope creep** (Path C): font shaping, fallback, IME, and ligature
  correctness are each substantial. Only take this on with intent.
- **terminfo drift** if you ship a custom entry — stick to `xterm-256color` unless
  you have a concrete reason.

---

## 20. References

- **DEC ANSI parser state machine** — Paul Williams, vt100.net (the canonical parser
  reference).
- **XTerm Control Sequences** (`ctlseqs`) — the de-facto sequence spec.
- **ECMA-48** — the underlying control-function standard.
- **esctest** / **vttest** — conformance test suites.
- **xterm.js** — emulator + addons (webgl, image, unicode11, search, web-links, fit).
- **alacritty_terminal**, **vte** (Rust), **libvterm** (C), **wezterm-term** — VT
  engines for a native core.
- **portable-pty** (Rust, wezterm), **node-pty** — PTY spawning.
- **cosmic-text**, **swash**, **harfbuzz**, **fontconfig** — text shaping/rasterizing/
  fallback for a native renderer; **wgpu**, **winit** — GPU + windowing.
- **Kitty keyboard protocol**, **CSI-u (fixterms)**, **modifyOtherKeys** — modern key
  encoding.
- **OSC 7** (cwd), **OSC 8** (hyperlinks), **OSC 52** (clipboard), **OSC 133**
  (semantic prompt) — shell-integration and feature sequences.
- **xdg-terminal-exec** — emerging freedesktop default-terminal spec.
- Prior art to read: **Alacritty**, **kitty**, **foot**, **wezterm**, **Hyper**
  (Electron + xterm.js proof), **Rio**.

---

*End of guide. Treat the **[Decision]** points as the agenda for your first design
session; everything else is a menu you can implement in milestone order.*
