# VT rendering smoke — vttest

[vttest](https://invisible-island.net/vttest/) is the classic **interactive**,
visually-inspected VT100/VT220 tester. Unlike esctest (which scores itself via
DECRQCRA), vttest draws test screens a human confirms by eye, so it's a manual
smoke rather than an automated gate.

## Running

```sh
# build vttest (not usually packaged with a runnable binary in-tree)
git clone https://github.com/ThomasDickey/vttest-snapshots.git
cd vttest-snapshots && ./configure && make        # -> ./vttest

# run it inside Sampa (a release bundle is self-contained; no vite needed)
/path/to/sampa -e /path/to/vttest
```

Then drive the menu: type a test number + Return, and Return to advance through
its sub-screens. The most informative screens:

- **1 — Cursor movements.** First screen draws an unbroken border of `*`/`+` around
  the 80-column edge with a centered frame of `E`s. Verifies cursor addressing,
  autowrap, and tab stops.
- **3 → 8 — VT100 character sets.** Shows US-ASCII, the national-replacement sets
  (e.g. British `£` for `#`), and **DEC Special Graphics** — the box-drawing
  glyphs. The classic corruption point.
- **4 — Double-sized characters** (DECDWL/DECDHL).
- **12 — Character attributes** (bold/underline/blink/reverse).

## Smoke result (2026-07, natural geometry, release binary)

Verified by screenshot against Sampa's WebGL renderer under Xvfb:

- ✅ **Main menu** renders crisp and aligned.
- ✅ **Cursor movements (test 1):** unbroken `*`/`+` border, `E`-frame exactly
  centered in 80 columns with one free position — matches vttest's own criterion.
- ✅ **Interactive menu navigation:** submenu entry and per-option state toggles
  (e.g. enabling NRC flips G1–G3 to British) re-render correctly.
- ✅ **VT100 character sets (3 → 8):** US-ASCII clean; British NRC shows `£`;
  **DEC Special Graphics line-drawing** (`┌┐└┘┼├┤┴┬│` + the `⎺⎻─⎼⎽` scan-lines and
  `≤≥π≠`) all render correctly.

No corruption observed in the exercised screens. Deeper sub-screens (double-size,
every attribute permutation) are left to a local manual pass — line-drawing and
attributes are also exercised continuously in the real-app matrix (tmux, the man
panel's boxes, `ls --color`).
