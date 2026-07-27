# VT conformance — esctest

Sampa is scored against [esctest2](https://github.com/ThomasDickey/esctest2)
(Thomas Dickey's maintained fork of the iTerm2 test suite). esctest drives the
terminal with escape sequences and reads the screen back to check what was
rendered.

The read-back is the interesting part: esctest verifies contents almost entirely
through **DECRQCRA** (request checksum of a rectangular area). A terminal that
doesn't answer DECRQCRA can't be scored at all — the suite times out on every
content assertion. Sampa therefore implements DECRQCRA in the frontend
(`src/main.ts`): it sums the code points of the requested cells (empty cells
count as space) and replies `DCS Pid ! ~ XXXX ST`. esctest reads one cell at a
time, so this per-cell sum equals the cell's character code, which is exactly
what esctest compares against (run with `--xterm-checksum 334`, the raw
non-negated convention).

## Running

```sh
# 1. an X server — Xvfb for headless / CI
Xvfb :99 -screen 0 1400x900x24 &
export DISPLAY=:99

# 2. a built Sampa (debug loads the frontend from the vite dev server, so run
#    `npm run dev` alongside; a release bundle is self-contained)
cargo build --manifest-path src-tauri/Cargo.toml

# 3. run the suite (fetches the pinned esctest2 on first use)
tools/conformance/run-esctest.sh                 # full suite
tools/conformance/run-esctest.sh --include CUP   # one feature
tools/conformance/run-esctest.sh --bin /path/to/sampa --display :99
```

The script prints the pass/known-bug/fail summary and groups the failures by
feature.

## Baseline (2026-07, natural geometry)

```
*** 305 tests passed, 43 known bugs, 220 TESTS FAILED ***
```

**305 passing** covers the core Sampa claims: cursor addressing (CUP/CUU/CUD/
CUF/CUB/CHA/HPA/VPA/HVP), erase (ED/EL/ECH), basic insert/delete (ICH/DCH/IL/DL),
tab stops, SGR, index/reverse-index/NEL, scrolling, save/restore cursor, and the
DECRQCRA screen read-back itself.

The 220 failures are dominated by **sequences Sampa deliberately does not
implement**, not regressions:

| Area | ~count | Why it fails |
|------|-------|--------------|
| `XtermWinops` | 28 | Window manipulation/reporting (iconify, move, report position) — not implemented; the webview has no meaningful X geometry. |
| `DECRQM` | 23 | Request-mode *reports* — xterm.js doesn't answer them. |
| `Change*Color` / `ResetColor` | 34 | OSC color set/**query** — no reply to `?` queries. |
| `DECDSR` | 10 | Extended device-status reports (printer/keyboard/…). |
| `DECSET` | 14 | DEC private modes xterm.js doesn't implement (e.g. left/right margins). |
| VT400 rect editing (`DECCRA`/`DECFRA`/`DECERA`/`DECSERA`/`DECIC`/`DECDC`/`DECBI`/`DECFI`) | ~40 | Only partially implemented by xterm.js. |
| `DA`/`DA2`, misc | ~10 | Device-attribute edge cases; a few scroll-region / reverse-wrap edge cases. |

A smaller tail is geometry-sensitive: esctest expects an 80×25 grid (it resizes
with `CSI 8;25;80t`), but xterm.js intercepts that window-op internally and
won't let the app pin the grid, so the suite runs at the window's natural size.
This affects only a minority of tests (most adapt via `CSI 18t`, which Sampa
*does* report).

## Release gate

Gate on the **absolute pass count**: a release must not drop below the recorded
baseline (**305**). That catches regressions in the sequences Sampa supports
without pretending to implement the query/report and VT400 features that make up
the expected-failure tail. Raise the baseline whenever a change legitimately
increases it.
