#!/usr/bin/env bash
#
# Run the esctest VT-conformance suite against Sampa and print the pass/fail
# summary. Sampa implements DECRQCRA (rectangular-area checksum), which is how
# esctest reads the screen back to verify rendered contents — without it the
# suite cannot score a webview terminal at all.
#
# Requirements: python3, git, an X server (use Xvfb for headless/CI), and a
# built Sampa binary. esctest2 is fetched (pinned) into tools/conformance/.esctest
# on first run.
#
# Usage:
#   tools/conformance/run-esctest.sh [--bin PATH] [--include REGEX] [--display :N]
#
# Examples:
#   # headless, full suite
#   Xvfb :99 -screen 0 1400x900x24 & export DISPLAY=:99
#   cargo build --manifest-path src-tauri/Cargo.toml   # (or use a release bin)
#   tools/conformance/run-esctest.sh
#
#   # just the cursor-position tests
#   tools/conformance/run-esctest.sh --include CUP
#
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
ESCTEST_DIR="$HERE/.esctest/esctest"
ESCTEST_REPO="https://github.com/ThomasDickey/esctest2.git"
ESCTEST_COMMIT="664be3cf2c1e3f06bc93a8bafb48a0db83c607db"   # pinned

BIN="$ROOT/src-tauri/target/debug/sampa-terminal"
INCLUDE=".*"
: "${DISPLAY:=:99}"

while [ $# -gt 0 ]; do
  case "$1" in
    --bin)     BIN="$2"; shift 2 ;;
    --include) INCLUDE="$2"; shift 2 ;;
    --display) DISPLAY="$2"; export DISPLAY; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }
command -v xdotool >/dev/null || { echo "xdotool required (window cleanup)" >&2; exit 1; }
[ -x "$BIN" ] || { echo "Sampa binary not found/executable: $BIN" >&2; exit 1; }

# Fetch the pinned esctest2 on first use.
if [ ! -d "$ESCTEST_DIR" ]; then
  echo "Fetching esctest2 (pinned $ESCTEST_COMMIT) ..."
  git clone --quiet "$ESCTEST_REPO" "$HERE/.esctest" || exit 1
  git -C "$HERE/.esctest" checkout --quiet "$ESCTEST_COMMIT" || exit 1
fi

LOG="$HERE/.esctest/esctest-$(printf '%s' "$INCLUDE" | tr -c 'A-Za-z0-9' _).log"
rm -f "$LOG"

# WebKitGTK software-render flags so it works under Xvfb.
export GDK_BACKEND=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 \
       WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1

for w in $(xdotool search --name '^esctestrun$' 2>/dev/null); do xdotool windowkill "$w" 2>/dev/null; done

# --window-id 0 makes esctest skip xwininfo (no real X window id needed).
# --xterm-checksum 334 selects the raw (non-negated) checksum convention Sampa
#   replies with, and empty cells compare as space.
nohup "$BIN" --title "esctestrun" --working-directory "$ESCTEST_DIR" \
  -e python3 esctest.py \
     --expected-terminal xterm --xterm-checksum 334 --max-vt-level 4 \
     --window-id 0 --timeout 1 --no-print-logs --logfile "$LOG" \
     --include "$INCLUDE" \
  > "$HERE/.esctest/last-run.out" 2>&1 &

echo "Running esctest (include='$INCLUDE') on DISPLAY=$DISPLAY ..."
for _ in $(seq 1 900); do
  sleep 1
  grep -q "passed," "$LOG" 2>/dev/null && break
done

for w in $(xdotool search --name '^esctestrun$' 2>/dev/null); do xdotool windowkill "$w" 2>/dev/null; done

SUMMARY="$(grep -E "\*\*\*.*passed" "$LOG" 2>/dev/null | tail -1)"
if [ -z "$SUMMARY" ]; then
  echo "No summary — esctest did not finish. See $LOG" >&2
  exit 1
fi
echo
echo "==== esctest summary (include='$INCLUDE') ===="
echo "$SUMMARY"
echo
echo "Failing tests grouped by feature:"
awk '/Failing tests:/{f=1;next} f && /Tests\./' "$LOG" | sed 's/\..*//' | sort | uniq -c | sort -rn
echo
echo "Full log: $LOG"
