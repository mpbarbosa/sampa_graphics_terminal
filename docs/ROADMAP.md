# Roadmap — Sampa Graphical Terminal

This roadmap expands the milestone skeleton in [DESIGN.md §18](DESIGN.md) into a
phased, actionable plan. It is written to stay **consistent with the decisions already
made** in the design guide: Path B (Tauri + xterm.js), a renderer-agnostic Rust core,
and the layered model (Layer 1 PTY → Layer 2 VT → Layer 3 renderer → Layer 4 input)
with a small, symmetric IPC seam.

**How to read this.** Milestones (M0–M5, v1) are sequential and each ends at a
demoable, shippable state. Within a milestone, *phases* group work that can largely
proceed in parallel. Every phase lists an **Exit criterion** — an observable,
testable condition — because "done" for a terminal means *verified against real
programs*, not "code written." Section references (`§n`) point at
[DESIGN.md](DESIGN.md).

Legend: ✅ done · 🔨 in progress · ⬜ not started.

---

## Guiding principles (apply to every phase)

1. **Core stays headless.** No Tauri/webview types leak into `crates/pty-core` or any
   future `src-core` service crate. If a feature needs the GUI, the logic still lives
   in the core and the frontend only renders/inputs (§4.1).
2. **The seam shape is fixed** (§9): commands frontend→core, events core→frontend,
   raw **bytes** over the wire for Path B. New capabilities extend this protocol;
   they don't invent a second channel.
3. **Security gates are authoritative in the core** (§13). The frontend can never
   bypass the preview allowlist or the OSC-52 clipboard gate.
4. **Every milestone lands its own tests** (§17). The parser/app-matrix/feature tests
   for a milestone ship *with* that milestone, not later.
5. **Prefer robustness sources over heuristics.** Where a feature can rest on OSC
   7/133 shell integration instead of grid-scraping, adopt the integration (§5.6,
   §10.2).

---

## M0 — Echo (proof of life) ✅ **shipped**

Window + PTY + xterm.js; type `ls`, see output; resize works. Validates Layers 1–3
and the seam.

- ✅ `pty-core`: spawn shell on a PTY, stream output, write input, resize, reap —
  verified against real zsh via `examples/echo.rs`.
- ✅ Tauri IPC bridge: `spawn_session` / `write_session` / `resize_session` +
  `pty://output/<id>` / `pty://exit/<id>` events (base64 across the boundary).
- ✅ xterm.js frontend wired to the core; FitAddon keeps the grid in sync.

**Exit criterion (met):** a native window runs a live zsh; typed commands echo and
run; window resize reaches the shell as SIGWINCH.

**Debt carried forward (address in M1):** single hard-coded session (no session
table beyond the id counter); no explicit teardown on window close; input is sent as
a JS string (`write_session(data: String)`) rather than raw bytes — fine for ASCII,
must move to a byte-safe channel for full keyboard encoding.

---

## M1 — A usable terminal 🔨

**Goal:** the "real terminal" contract (§3). You can live in this terminal for
everyday work: run vim/htop/tmux, use job control, select and paste correctly, and
exit cleanly. This is mostly **input-encoding correctness** — budget the most time
here (§19).

> **Implementation status.** The core + bridge + frontend for M1 have landed and
> been verified live (headless, on Xvfb + xdotool):
> - ✅ Full stack boots; live zsh renders; interactive `echo` round-trip with real
>   `$SHELL` expansion.
> - ✅ **vim**: alt-screen enter (insert/`Esc`, status line) and **restore** on `:q!`.
> - ✅ **Ctrl-C** interrupts the current line (SIGINT reaches the foreground group).
> - ✅ **Selection + Ctrl-Shift-C** copy (`navigator.clipboard.writeText`).
> - ✅ **Paste-safety**: multi-line paste → confirmation modal → Cancel aborts /
>   Accept inserts as **bracketed paste that does not auto-execute** (§13).
> - ✅ Exit event renders the colored `[Success]` / code line.
>
> A bug was found and fixed during verification: `window.confirm()` is a silent
> no-op in the Tauri WebKitGTK webview, and Ctrl-Shift-V fires a native DOM paste
> that bypassed it. Paste-safety was rewritten to intercept the `paste` event in
> the capture phase (covers Ctrl-Shift-V / middle-click / Shift-Insert) and gate it
> through an in-DOM modal — no reliance on the flaky async clipboard-read API.
>
> **Still to confirm on a real display** (synthetic input made these unreliable to
> drive headless): mouse reporting in tmux/htop (§1.3), and clipboard **read**
> reliability across paste routes. If read proves gated in practice, switch to the
> Tauri clipboard plugin — tracked as an M2 follow-up.

### Phase 1.1 — Session & lifecycle hardening
- Promote the ad-hoc id counter into a real **session table** (`session_id →
  {pty, meta}`) in the core, anticipating tabs (§4.3). Keep it in the core, not
  `src-tauri`.
- Reap on window/tab close: add a `close_session` command; drop the `PtyHandle` and
  join the reader thread. Emit `exit` with code/signal (extend the `pty://exit`
  event to carry `{code, signal}` per §9).
- Surface child **exit status** (code vs. signal) instead of the current bare exit.

**Exit criterion:** opening and closing many sessions leaks no threads or zombie
processes (verify with `ps`/thread count); a shell that exits non-zero reports it.

### Phase 1.2 — Keyboard → bytes (the correctness sink, §8.1)
- Rely on xterm.js for base encoding but **verify** the full matrix: printable +
  IME/dead keys/compose, C0 controls, special keys under **DECCKM** (application
  cursor keys) and keypad mode.
- Modifier encodings: support at least **CSI-u (fixterms)** / `modifyOtherKeys`;
  **kitty keyboard protocol** is a stretch goal that unblocks neovim power-mappings.
- **Bracketed paste (mode 2004):** wrap pastes in `ESC[200~ … ESC[201~`.
- Move the write path to **raw bytes** end-to-end (frontend sends bytes; the Tauri
  command takes `Vec<u8>` / base64, not a `String`) so non-UTF-8 and control bytes
  survive.

**Exit criterion:** in `vim` and `neovim`, arrows/Home/End/PgUp, F-keys, and
`Ctrl`/`Alt`/`Shift` chords all behave; a multi-line paste appears as text, never
executes.

### Phase 1.3 — Mouse (§8.2)
- Encode mouse when a mode is enabled (1000/1002/1003), preferring **SGR (1006)**.
- When mouse mode is off, the mouse drives **local selection** instead.

**Exit criterion:** `htop` and `tmux` mouse interactions work; text selection works
when the app isn't grabbing the mouse.

### Phase 1.4 — Scrollback, selection, clipboard (§8.3)
- Capped scrollback ring buffer (config comes in M2; ship a sane default now).
- Selection: char (drag), word (double), line (triple); block selection with a
  modifier.
- Copy `Ctrl-Shift-C`, paste `Ctrl-Shift-V`; on X11, middle-click → PRIMARY.
- Paste safety: strip embedded `ESC[201~`; **confirm multi-line pastes** (§13).

**Exit criterion:** copy/paste round-trips across apps; PRIMARY works on X11; a
pasted payload containing a newline prompts for confirmation.

### Phase 1.5 — Alt-screen apps, signals, job control
- Alt-screen (1049) apps render and restore the main screen on exit.
- Terminal signals via the controlling terminal: `Ctrl-C`→SIGINT, `Ctrl-Z`→SIGTSTP,
  `Ctrl-\`→SIGQUIT; `bg`/`fg`/`jobs` work (portable-pty already gives a controlling
  terminal — verify, don't assume).

**Exit criterion — the M1 app matrix (§17):** `vim`, `neovim`, `tmux`, `htop`,
`less`, `git log`, `python`/`ipython` each render without corruption, respond to
resize, and honor Ctrl-C/Ctrl-Z. Clean exit returns to a healthy prompt.

**M1 tests to land:** first parser golden-snapshot fixtures; the app-matrix smoke
checklist; a paste-safety test.

---

## M2 — Comfort ✅

**Goal:** make it pleasant and configurable for daily use. Introduces the
configuration model (§11) and the first multi-session UI.

> **Implementation status — complete; all three phases verified live.**
> - ✅ **2.1 Config** — headless `crates/config` (`sampa-config`): serde/TOML `Config`
>   with per-field defaults, XDG path resolution, `deny_unknown_fields` so typos
>   error, validation, shipped documented default (7 unit tests). Bridge loads at
>   startup (writes default to `$XDG_CONFIG_HOME/sampa/config.toml` on first run),
>   `--config` override, `get_config`, `notify` watcher → `config://changed`.
> - ✅ **2.2 Theming/fonts/cursor** — frontend builds xterm from config and
>   re-applies theme / font / cursor / scrollback / padding live; config-gated visual
>   bell. (Verified: a config edit hot-reloaded a light theme + size-20 font + bar
>   cursor into a running window.)
> - ✅ **2.3 Tabs + search + keybindings** — config-driven keybinding table
>   (`Ctrl+Shift+*`, layout-independent code matching); tabs on the M1 session table
>   (new/close/next/prev, OSC titles, chrome-free single tab, close reaps the child,
>   last-tab-close quits via `quit_app`); incremental search (xterm search addon)
>   with a find overlay; font zoom. (Verified live: two tabs, switch, search-highlight,
>   close-reaps 2→1, quit on last close.)
> - Deferred: **splits** (roadmap-optional); **ligatures** toggle (config field exists,
>   needs the xterm ligatures addon). OSC 4/10/11 dynamic colors are xterm built-ins.

### Phase 2.1 — Configuration model (§11)
- TOML at `$XDG_CONFIG_HOME/sampa/config.toml` (fallback `~/.config/...`), **live
  reload** on change, `--config FILE` override.
- Sections: `font`, `colors`, `window`, `scrollback`, `shell`, `cursor`,
  `keybindings`, `bell`, feature toggles.
- Ship a **documented default config**. Config parsing/validation lives in the core.

**Exit criterion:** editing the config file live-updates a running window; an invalid
config surfaces a clear error and falls back to defaults rather than crashing.

### Phase 2.2 — Theming, fonts, cursor
- 16 ANSI + default fg/bg/cursor/selection; truecolor passthrough; **OSC 4/10/11**
  dynamic color set/query (§7.3).
- Font family + size + fallback chain, line height; **ligatures toggle** (default
  off, §7.3). Box-drawing/Powerline glyph alignment.
- Cursor shape + blink; visual and/or audible **bell**.

**Exit criterion:** switching themes/fonts via config takes effect on reload;
truecolor and a Powerline prompt render correctly.

### Phase 2.3 — Tabs, (optional) splits, search
- **Keybinding table** (§8.4): chords → actions, with a reserved `Ctrl-Shift-*`
  namespace so shell keys aren't shadowed. Unbound chords fall through to the PTY.
- Tabs backed by the M1 session table; new-tab/close-tab/next/prev. Splits optional.
- In-terminal **search** over scrollback (xterm search addon).

**Exit criterion:** multiple tabs each run independent shells; closing a tab reaps
its child; search finds and highlights matches in scrollback.

**M2 tests to land:** config load/validate/live-reload tests; keybinding
fall-through test (an unbound chord reaches the shell as bytes).

---

## M3 — Linux citizen 🔨

**Goal:** it behaves like *the* terminal — launchers can open it, other apps can run
commands in it, and it installs cleanly (§12, §16). No signature features yet, but
after M3 the app is a legitimate daily driver.

> **Implementation status.**
> - ✅ **3.1 CLI contract** — new headless `crates/cli` (`sampa-cli`): infallible argv
>   parser for `-e`/`--`, `--working-directory`/`-w`, `--title`/`-T`, `--hold`,
>   `--login`/`-l`, `--class`, `--config`, `-h`/`-V` (8 unit tests). Bridge applies the
>   overrides to the *first* session only (new tabs open a normal shell), sets the
>   window title, and exposes `get_launch_options` (hold/title) to the frontend, which
>   keeps a `--hold` tab open on exit. **Verified live** in one launch:
>   `--hold --title "M3 Test" --working-directory=/tmp -e sh -c "pwd; echo …"` printed
>   `/tmp` + the echo and held with a green status line; title applied.
>   - Also fixed a latent **output race** with a ready-gate handshake
>     (`session_ready`): a fast `-e` command used to exit before the frontend attached
>     listeners, losing its output. Now the backend parks each session's pump until the
>     frontend is listening.
>   - `--class` is parsed but the runtime `WM_CLASS` override isn't applied yet (tao
>     limitation); tracked below.
> - 🔨 **3.2 Desktop entry** — `packaging/sampa.desktop` (TerminalEmulator category,
>   `Exec=… %F`, keywords) wired as the deb `desktopTemplate`; binary renamed to
>   `sampa` via `mainBinaryName`. `x-terminal-emulator`/`xdg-terminal-exec`
>   registration is documented but needs the built `.deb` to exercise.
> - ⬜ **3.3 Packaging** — bundle config is in place; the actual `npm run tauri build`
>   (AppImage/.deb) hasn't been run (heavy) — next increment. `StartupWMClass` and the
>   installed command name should be verified against the real bundle.

### Phase 3.1 — The command-line contract (§12.2)
- `sampa` (default shell), `-e CMD…` and `-- CMD…` (run CMD instead of shell — `-e`
  is what `x-terminal-emulator -e` uses), `--working-directory=DIR`, `--title=STR`,
  `--hold`, `--class=STR` (WM_CLASS), `--config=FILE`. Respect `$SHELL`; login toggle.

**Exit criterion:** `sampa -e htop`, `sampa --working-directory=/tmp`, and `--hold`
each behave as specified; WM_CLASS is set (verify with `xprop`).

### Phase 3.2 — Desktop entry & default-terminal registration (§12.3)
- Install `sampa.desktop` with `Categories=System;TerminalEmulator;`.
- Debian/Ubuntu `update-alternatives` for `x-terminal-emulator`; provide/participate
  in **`xdg-terminal-exec`**. Document the messy **GNOME** default-terminal story
  honestly — don't promise one-click.

**Exit criterion:** the app appears in the desktop menu; on a Debian/XFCE target it
can be selected as the preferred terminal and `x-terminal-emulator -e` opens it.

### Phase 3.3 — Packaging & distribution (§16)
- **AppImage** (portable) and **.deb** (registers `x-terminal-emulator`) first.
- Rely on system `xterm-256color` terminfo; graceful fallback over ssh.
- Decide the **Flatpak vs. "run the host shell" [Decision]** (§19) — Flatpak
  sandboxing fights a terminal's purpose; document the trade-off before committing.
- CI build matrix for **X11 + Wayland** (§16).

**Exit criterion:** a freshly installed .deb on a clean VM launches, registers as a
terminal alternative, and runs the host shell; the AppImage runs with no install.

---

## M4 — Signature features ⬜

**Goal:** the product differentiators (§10) — palette, live man panel, safe preview —
as **in-process core services** with zero network surface. This is where Sampa stops
being "another terminal."

### Phase 4.0 — Shell integration substrate (do this first)
- Adopt **OSC 133** semantic-prompt marks and **OSC 7** cwd reporting (§5.6). This
  turns the man/preview command detection from fragile grid-scraping into exact
  prompt/command boundaries (§10.2, §19). Ship opt-in shell hooks for zsh/bash;
  keep grid-scraping as the documented fallback.
- Core learns each session's **cwd** (from OSC 7, else `/proc/<pid>/cwd`) and
  **current command line** — both feed all three features.

**Exit criterion:** the core emits accurate `cwd` and prompt-boundary events for a
zsh session with the hooks installed; without hooks, the fallback still yields a
best-effort command token.

### Phase 4.1 — Command palette (§10.1)
- Core service: enumerate `$PATH` executables (execute bit, dedupe, sort), cache with
  TTL, invalidate on `$PATH` change.
- `Ctrl-Shift-P` opens a fuzzy overlay; Enter **inserts** `"<cmd> "` at the prompt —
  **never auto-runs**. Escape closes.

**Exit criterion:** palette opens, filters as you type, and inserts (does not
execute) the chosen command; the executable list refreshes when `$PATH` changes.

### Phase 4.2 — Live manual panel (§10.2)
- Detect the command token (cursor-column aware so shell autosuggestions past the
  cursor are excluded; walk up to the prompt line for multi-line constructs), debounce
  ~300 ms.
- Render `man -P cat <cmd>` via an **arg vector, no shell**; validate `cmd` against
  `^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$`; strip nroff overstrike + ANSI; cache per
  command.
- **Collapse the panel** for keywords (`for`) or no-man commands — never leave a stale
  page.

**Exit criterion (§17):** the panel opens for real commands and **closes** for
keywords/no-man; it tracks the token as you type without flicker.

### Phase 4.3 — Safe auto-run preview (§10.3) — highest risk, safety-first
- Detect the typed line (same cursor-column technique), debounce ~550 ms.
- **Authoritative in-core gate:** (1) syntax check `zsh -n -c <line>` (never executes;
  incomplete lines aren't run); (2) safety classify — reject shell metacharacters
  (`; & < > \` $() && || >>`), env assignments, and anything off a small **read-only
  allowlist**; `git` read-only subcommands only; reject `find -exec/-delete`,
  `tail -f`; (3) execute the survivor via `execFile(zsh, ["-c", line])` with timeout +
  SIGKILL, output cap, **stdin closed**, cwd = session dir.
- **Clear the preview on Enter** (watch input for `\r`). Global on/off toggle,
  default on. Document that "read-only" is a conservative allowlist, not a proof.

**Exit criterion (§13, §17 — filesystem-verified):** a typed `rm <file>` (and `> f`,
`git commit`, `find … -delete`) never runs — the file is provably untouched; a valid
`ls -l` previews; the preview **clears on Enter**. This test gates the milestone.

**M4 tests to land:** palette-inserts-not-runs; man opens/closes correctly; the
preview allowlist test suite (writes refused, reads allowed, clears on Enter).

---

## M5 — Polish & scale ⬜

**Goal:** performance, graphics, and conformance to a shippable bar (§14, §17).

### Phase 5.1 — Rendering performance (§14)
- **WebGL addon** for glyph throughput; off-thread PTY read → batched parse → **one
  render per vsync**; damage-based drawing; coalesce output bursts.
- Track benchmarks in CI: `time cat 50MB.log` throughput, added input latency
  (< one frame), `yes | head -c 100M` flood stability, 100k-line scrollback memory.

**Exit criterion:** flood tests stay responsive (no UI freeze); input latency and
throughput hit the targets recorded in CI trend lines.

### Phase 5.2 — Graphics & links
- **Images** (sixel and/or kitty protocol) with **caps on dimensions, memory, and
  count** (§13 OOM guard). Pixel geometry already plumbed via `resize` xpixel/ypixel.
- **OSC 8 hyperlinks:** never auto-open — explicit click, show the target (§13).

**Exit criterion:** a sixel image renders within caps; a malicious oversized image is
rejected, not OOM; hyperlinks require a click and display their destination.

### Phase 5.3 — Conformance & hardening
- Run **esctest**; track pass rate and **gate releases on a threshold** for claimed
  sequences. **vttest** manual smoke.
- Escape-sequence hardening pass (§13): OSC 52 clipboard **write gate** (reads off by
  default), title (OSC 0/2) sanitization, safe fixed replies to DECRQSS/DA/color
  queries.
- Additional packaging: **.rpm**, and Flatpak *if* the M3 decision went that way.

**Exit criterion:** esctest meets the agreed threshold; the OSC-52 gate prompts (and
denies by default) — verified by test; no query sequence echoes attacker-controlled
bytes back to input.

---

## v1 — Release ⬜

The release bar, per §18:

- ✅ Real-app matrix green (§17): vim, neovim, tmux, htop, less, mc, git log, ssh,
  weechat, emacs -nw, ipython, fzf, truecolor, sixel, CJK/emoji width.
- ✅ esctest threshold met; vttest smoke clean.
- ✅ Latency/throughput targets met and trending in CI.
- ✅ Signature-feature tests green (palette inserts, man opens/closes, preview refuses
  writes + clears on Enter).
- ✅ Config reference + user docs complete; default config and themes shipped.
- ✅ AppImage + .deb (+ .rpm) published; `.desktop` and `x-terminal-emulator`
  registration verified on at least one Debian-family and one non-GNOME desktop.

---

## Cross-cutting tracks (run continuously, not a milestone)

- **Testing (§17):** every milestone adds its own layer — parser goldens (M1+),
  app-matrix (M1+), feature tests (M4), perf regression (M5). Never let a milestone
  ship without its tests.
- **Security (§13):** the preview allowlist and OSC-52 gate are authoritative in the
  core and covered by tests from the moment each feature lands. Keep **zero network
  surface** — features are in-process function calls, never ports.
- **ADRs (docs/):** record each **[Decision]** as it's resolved — the Flatpak
  distribution story (M3), kitty-keyboard support (M1), grid-scrape vs. OSC-133
  reliance (M4), and any native-renderer (Path C) decision.
- **Docs:** keep `README.md` Status and `CLAUDE.md` current as milestones land.

---

## Sequencing & dependencies (at a glance)

```
M0 ✅ ──► M1 (usable) ──► M2 (comfort) ──► M3 (Linux citizen) ──► M4 (signature) ──► M5 (polish) ──► v1
                │                              │                     ▲
                │ session table                │ config toggles      │ OSC 7/133 (Phase 4.0)
                └──────────────► reused by tabs (M2) & sessions ─────┘ makes man/preview robust
```

- M1's **session table** is the substrate for M2 tabs and all later per-session
  features — build it right in M1.
- M2's **config + keybinding table** is where M4's feature toggles and the palette
  hotkey plug in — leave the seams.
- M4 **Phase 4.0 (OSC 7/133) comes before** the palette/man/preview phases; it's the
  difference between robust and best-effort (§19).
- The **[Decision] points** (§4.2 Path B, §16 Flatpak, native-renderer Path C) are
  design gates, not code phases — resolve and record them (ADRs) before the milestone
  that depends on them.

> **Realistic pacing (§18):** M0 in days (done); M1 in a few weeks (input-encoding
> correctness dominates); M2–M3 a few weeks each; M4 builds directly on the earlier
> browser-prototype logic; M5 is open-ended. Treat these as ordering, not deadlines.
