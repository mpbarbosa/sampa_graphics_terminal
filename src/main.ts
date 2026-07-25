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
  // Nerd Fonts first so Powerline separators + prompt icons (p10k, starship) render
  // instead of tofu; plain monospace fallbacks keep it working without them (M2 makes
  // this configurable).
  fontFamily:
    '"MesloLGS NF", "Hack Nerd Font", ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace',
  fontSize: 14,
  cursorBlink: true,
  allowProposedApi: true,
  // Capped scrollback ring (DESIGN.md §14); made configurable in M2.
  scrollback: 10000,
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

const encoder = new TextEncoder();

// PTY output arrives base64-encoded (raw bytes, possibly partial UTF-8); decode to
// a Uint8Array and let xterm.js reassemble multi-byte/escape sequences.
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// Input goes the other way: encode to raw bytes then base64 so control sequences
// and non-UTF-8 key encodings survive the JS↔Rust boundary intact (DESIGN.md §8.1).
function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

const session = await invoke<number>("spawn_session", {
  cols: term.cols,
  rows: term.rows,
});

function sendInput(data: string): void {
  void invoke("write_session", { session, data: bytesToB64(encoder.encode(data)) });
}

await listen<string>(`pty://output/${session}`, (e) =>
  term.write(b64ToBytes(e.payload)),
);

interface ExitPayload {
  code: number;
  success: boolean;
  detail: string;
}
await listen<ExitPayload>(`pty://exit/${session}`, (e) => {
  const { code, success, detail } = e.payload;
  const color = success ? "32" : "31"; // green / red
  term.write(`\r\n\x1b[${color}m[${detail || `process exited (code ${code})`}]\x1b[0m\r\n`);
});

// Keystrokes / bracketed-paste bytes from xterm -> shell. xterm wraps pastes in the
// ESC[200~ … ESC[201~ markers itself when the app enables mode 2004.
term.onData(sendInput);

// A promise-based confirmation modal rendered inside the webview. We do NOT use
// window.confirm(): in the Tauri WebKitGTK webview it silently returns true without
// showing a dialog, so it can't gate anything. An in-DOM overlay also matches the
// renderer-owns-overlays approach the M4 panels will use.
function confirmModal(message: string, okLabel = "Paste"): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.innerHTML = `
      <div class="modal-box" role="alertdialog" aria-modal="true">
        <p class="modal-msg"></p>
        <div class="modal-actions">
          <button class="modal-btn" data-act="cancel">Cancel</button>
          <button class="modal-btn modal-btn-primary" data-act="ok"></button>
        </div>
      </div>`;
    overlay.querySelector(".modal-msg")!.textContent = message;
    const okBtn = overlay.querySelector('[data-act="ok"]') as HTMLButtonElement;
    okBtn.textContent = okLabel;

    const done = (result: boolean) => {
      window.removeEventListener("keydown", onKey, true);
      overlay.remove();
      term.focus();
      resolve(result);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        done(false);
      } else if (e.key === "Enter") {
        e.preventDefault();
        done(true);
      }
    };
    window.addEventListener("keydown", onKey, true);
    overlay.addEventListener("click", (e) => {
      const act = (e.target as HTMLElement).dataset.act;
      if (act === "ok") done(true);
      else if (act === "cancel" || e.target === overlay) done(false);
    });

    document.body.append(overlay);
    okBtn.focus();
  });
}

// Paste-safety (DESIGN.md §8.3, §13). We intercept the DOM `paste` event in the
// capture phase rather than reading the clipboard ourselves: WebKitGTK fires a
// native paste on Ctrl-Shift-V / Shift-Insert / middle-click, and xterm's built-in
// handler would otherwise paste straight through, bypassing confirmation. Reading
// `clipboardData` here is synchronous and needs no async-clipboard permission.
let pasting = false;
async function handlePaste(text: string): Promise<void> {
  if (!text || pasting) return;
  // Confirm multi-line pastes: a stray newline runs a command (paste injection).
  const lines = text.split("\n").length;
  if (lines > 1) {
    const ok = await confirmModal(
      `Paste ${lines} lines? A pasted newline will run the command.`,
    );
    if (!ok) return;
  }
  pasting = true;
  // term.paste applies bracketed-paste wrapping and strips the end marker.
  term.paste(text);
  pasting = false;
}

const termEl = document.getElementById("terminal")!;
termEl.addEventListener(
  "paste",
  (e: ClipboardEvent) => {
    // Stop xterm's own paste; we own the gating.
    e.preventDefault();
    e.stopPropagation();
    void handlePaste(e.clipboardData?.getData("text") ?? "");
  },
  true, // capture: run before xterm's listener on the inner textarea
);

// Copy on the reserved Ctrl-Shift-* namespace so shell keybindings aren't shadowed
// (DESIGN.md §8.4). Returning false stops xterm from also treating it as input.
// (Ctrl-Shift-V is handled by the native paste event above, not here.)
term.attachCustomKeyEventHandler((e): boolean => {
  if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyC") {
    const sel = term.getSelection();
    if (sel) {
      void navigator.clipboard.writeText(sel);
      return false;
    }
  }
  return true;
});

// Keep the PTY grid size in sync with the rendered terminal.
function syncSize(): void {
  fit.fit();
  void invoke("resize_session", { session, cols: term.cols, rows: term.rows });
}
new ResizeObserver(syncSize).observe(document.getElementById("terminal")!);
window.addEventListener("resize", syncSize);
requestAnimationFrame(syncSize);

// Best-effort teardown so the shell is reaped promptly on window/tab close.
window.addEventListener("beforeunload", () => {
  void invoke("close_session", { session });
});
