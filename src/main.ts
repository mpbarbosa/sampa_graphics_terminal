// Frontend (Layers 3–4): renders the terminal with xterm.js and bridges it to the
// Rust PTY core over Tauri IPC. Keeping this thin is deliberate — all terminal
// behavior lives in pty-core so the renderer can be replaced later.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const term = new Terminal({
  fontFamily: 'ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace',
  fontSize: 14,
  cursorBlink: true,
  allowProposedApi: true,
  theme: {
    background: "#16161e",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    black: "#15161e",
    red: "#f7768e",
    green: "#9ece6a",
    yellow: "#e0af68",
    blue: "#7aa2f7",
    magenta: "#bb9af7",
    cyan: "#7dcfff",
    white: "#a9b1d6",
  },
});

const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById("terminal")!);
fit.fit();
term.focus();

// PTY output arrives base64-encoded (raw bytes, possibly partial UTF-8); decode to
// a Uint8Array and let xterm.js reassemble multi-byte/escape sequences.
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const session = await invoke<number>("spawn_session", {
  cols: term.cols,
  rows: term.rows,
});

await listen<string>(`pty://output/${session}`, (e) =>
  term.write(b64ToBytes(e.payload)),
);
await listen(`pty://exit/${session}`, () =>
  term.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n"),
);

// Keystrokes / pasted text -> shell.
term.onData((data) => void invoke("write_session", { session, data }));

// Keep the PTY grid size in sync with the rendered terminal.
function syncSize(): void {
  fit.fit();
  void invoke("resize_session", { session, cols: term.cols, rows: term.rows });
}
new ResizeObserver(syncSize).observe(document.getElementById("terminal")!);
window.addEventListener("resize", syncSize);
requestAnimationFrame(syncSize);
