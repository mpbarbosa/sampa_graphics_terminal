// Frontend (Layers 3–4): renders terminals with xterm.js and bridges them to the
// Rust cores over Tauri IPC. Deliberately thin — all terminal/session behavior lives
// in the headless crates. M2 adds config-driven theming, tabs, search, and keybinds.

import { Terminal, type ITerminalOptions, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// Mirror of sampa_config::Config (the subset the renderer applies). The core owns
// parsing/validation/defaults; here we just map it onto xterm.js (DESIGN.md §11).
interface Config {
  font: { family: string; size: number; ligatures: boolean };
  colors: Record<string, string>;
  window: { padding_x: number; padding_y: number; cols: number; rows: number };
  scrollback: { lines: number };
  cursor: { style: "block" | "bar" | "underline"; blink: boolean };
  bell: { visual: boolean; audible: boolean };
  keybindings: Record<string, string>;
}

const encoder = new TextEncoder();

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

function toTheme(c: Record<string, string>): ITheme {
  return {
    background: c.background,
    foreground: c.foreground,
    cursor: c.cursor,
    selectionBackground: c.selection,
    black: c.black,
    red: c.red,
    green: c.green,
    yellow: c.yellow,
    blue: c.blue,
    magenta: c.magenta,
    cyan: c.cyan,
    white: c.white,
    brightBlack: c.bright_black,
    brightRed: c.bright_red,
    brightGreen: c.bright_green,
    brightYellow: c.bright_yellow,
    brightBlue: c.bright_blue,
    brightMagenta: c.bright_magenta,
    brightCyan: c.bright_cyan,
    brightWhite: c.bright_white,
  };
}

let currentCfg: Config = await invoke<Config>("get_config");
let fontZoom = 0; // added to the configured size by zoom keybinds; reset to 0

function effectiveFontSize(): number {
  return Math.max(4, currentCfg.font.size + fontZoom);
}

function xtermOptions(cfg: Config): ITerminalOptions {
  return {
    allowProposedApi: true,
    fontFamily: cfg.font.family,
    fontSize: effectiveFontSize(),
    cursorStyle: cfg.cursor.style,
    cursorBlink: cfg.cursor.blink,
    scrollback: cfg.scrollback.lines,
    theme: toTheme(cfg.colors),
  };
}

// ── Keybindings ────────────────────────────────────────────────────────────────
// Chords are matched on physical key *codes* so they're layout-independent.
interface Chord {
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
  code: string | null;
}

function tokenToCode(tok: string): string | null {
  if (/^[A-Za-z]$/.test(tok)) return "Key" + tok.toUpperCase();
  if (/^[0-9]$/.test(tok)) return "Digit" + tok;
  const named: Record<string, string> = {
    Right: "ArrowRight",
    Left: "ArrowLeft",
    Up: "ArrowUp",
    Down: "ArrowDown",
    Tab: "Tab",
    Enter: "Enter",
    Space: "Space",
    Equal: "Equal",
    Plus: "Equal",
    Minus: "Minus",
    Backspace: "Backspace",
  };
  return named[tok] ?? null;
}

function parseChord(s: string): Chord {
  const parts = s.split("+").map((p) => p.trim());
  const chord: Chord = { ctrl: false, shift: false, alt: false, meta: false, code: null };
  for (const p of parts) {
    const lower = p.toLowerCase();
    if (lower === "ctrl" || lower === "control") chord.ctrl = true;
    else if (lower === "shift") chord.shift = true;
    else if (lower === "alt") chord.alt = true;
    else if (lower === "meta" || lower === "super" || lower === "cmd") chord.meta = true;
    else chord.code = tokenToCode(p);
  }
  return chord;
}

function chordMatches(c: Chord, e: KeyboardEvent): boolean {
  return (
    c.code !== null &&
    e.code === c.code &&
    e.ctrlKey === c.ctrl &&
    e.shiftKey === c.shift &&
    e.altKey === c.alt &&
    e.metaKey === c.meta
  );
}

// ── Tabs ─────────────────────────────────────────────────────────────────────
interface Tab {
  id: number; // session id
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
  pane: HTMLElement;
  tabEl: HTMLElement;
  titleEl: HTMLElement;
  unlisten: UnlistenFn[];
}

const appEl = document.getElementById("app")!;
const tabbarEl = document.getElementById("tabbar")!;
const contentEl = document.getElementById("content")!;

const tabs: Tab[] = [];
let active = -1;

function activeTab(): Tab | undefined {
  return tabs[active];
}

const newTabBtn = document.createElement("button");
newTabBtn.className = "tab-new";
newTabBtn.textContent = "+";
newTabBtn.title = "New tab";
newTabBtn.addEventListener("click", () => void createTab());
tabbarEl.append(newTabBtn);

function flash(el: HTMLElement): void {
  el.classList.add("bell-flash");
  setTimeout(() => el.classList.remove("bell-flash"), 100);
}

function fitAndReport(t: Tab): void {
  t.fit.fit();
  void invoke("resize_session", { session: t.id, cols: t.term.cols, rows: t.term.rows });
}

function activate(i: number): void {
  if (i < 0 || i >= tabs.length) return;
  active = i;
  tabs.forEach((t, idx) => {
    t.pane.hidden = idx !== i;
    t.tabEl.classList.toggle("active", idx === i);
  });
  const t = tabs[i];
  fitAndReport(t);
  t.term.focus();
}

function switchTab(delta: number): void {
  if (tabs.length > 1) activate((active + delta + tabs.length) % tabs.length);
}

function refreshChrome(): void {
  // Hide the tab bar entirely when there's a single tab (chrome-free like M1).
  appEl.classList.toggle("single-tab", tabs.length <= 1);
}

async function createTab(): Promise<void> {
  const pane = document.createElement("div");
  pane.className = "term-pane";
  contentEl.append(pane);

  const term = new Terminal(xtermOptions(currentCfg));
  const fit = new FitAddon();
  const search = new SearchAddon();
  term.loadAddon(fit);
  term.loadAddon(search);
  term.open(pane);
  pane.style.padding = `${currentCfg.window.padding_y}px ${currentCfg.window.padding_x}px`;
  fit.fit();

  const id = await invoke<number>("spawn_session", { cols: term.cols, rows: term.rows });

  const unlisten: UnlistenFn[] = [];
  unlisten.push(
    await listen<string>(`pty://output/${id}`, (e) => term.write(b64ToBytes(e.payload))),
  );
  unlisten.push(await listen(`pty://exit/${id}`, () => closeTabById(id)));

  term.onData((data) =>
    void invoke("write_session", { session: id, data: bytesToB64(encoder.encode(data)) }),
  );
  term.onBell(() => {
    if (currentCfg.bell.visual) flash(pane);
  });

  // Tab bar entry (inserted before the trailing "+" button).
  const tabEl = document.createElement("div");
  tabEl.className = "tab";
  tabEl.setAttribute("role", "tab");
  const titleEl = document.createElement("span");
  titleEl.className = "tab-title";
  titleEl.textContent = "shell";
  const closeEl = document.createElement("button");
  closeEl.className = "tab-close";
  closeEl.textContent = "✕";
  closeEl.title = "Close tab";
  tabEl.append(titleEl, closeEl);
  tabbarEl.insertBefore(tabEl, newTabBtn);

  const tab: Tab = { id, term, fit, search, pane, tabEl, titleEl, unlisten };

  term.onTitleChange((t) => {
    titleEl.textContent = t || "shell";
    tabEl.title = t;
  });
  tabEl.addEventListener("mousedown", (e) => {
    if (e.target === closeEl) return;
    activate(tabs.indexOf(tab));
  });
  closeEl.addEventListener("click", (e) => {
    e.stopPropagation();
    closeTab(tabs.indexOf(tab));
  });

  tabs.push(tab);
  refreshChrome();
  activate(tabs.length - 1);
}

function closeTab(i: number): void {
  const t = tabs[i];
  if (!t) return;
  t.unlisten.forEach((u) => u());
  void invoke("close_session", { session: t.id });
  t.term.dispose();
  t.pane.remove();
  t.tabEl.remove();
  tabs.splice(i, 1);

  if (tabs.length === 0) {
    // Last tab closed → quit, like a normal terminal.
    void invoke("quit_app");
    return;
  }
  refreshChrome();
  activate(Math.min(i, tabs.length - 1));
}

function closeTabById(id: number): void {
  const i = tabs.findIndex((t) => t.id === id);
  if (i >= 0) closeTab(i);
}

// ── Config (live reload) ─────────────────────────────────────────────────────
function applyToAllTabs(): void {
  for (const t of tabs) {
    t.term.options.fontFamily = currentCfg.font.family;
    t.term.options.fontSize = effectiveFontSize();
    t.term.options.cursorStyle = currentCfg.cursor.style;
    t.term.options.cursorBlink = currentCfg.cursor.blink;
    t.term.options.scrollback = currentCfg.scrollback.lines;
    t.term.options.theme = toTheme(currentCfg.colors);
    t.pane.style.padding = `${currentCfg.window.padding_y}px ${currentCfg.window.padding_x}px`;
  }
  const t = activeTab();
  if (t) fitAndReport(t);
}

void listen<Config>("config://changed", (e) => {
  currentCfg = e.payload;
  rebuildBindings();
  applyToAllTabs();
});
void listen<string>("config://error", (e) =>
  console.warn("[sampa] config error:", e.payload),
);

function zoom(delta: number | null): void {
  fontZoom = delta === null ? 0 : fontZoom + delta;
  applyToAllTabs();
}

// ── Search ───────────────────────────────────────────────────────────────────
const searchEl = document.getElementById("search")!;
const searchInput = document.getElementById("search-input") as HTMLInputElement;

function runSearch(forward: boolean): void {
  const t = activeTab();
  const q = searchInput.value;
  if (!t || !q) return;
  if (forward) t.search.findNext(q);
  else t.search.findPrevious(q);
}
function openSearch(): void {
  searchEl.hidden = false;
  searchInput.select();
  searchInput.focus();
}
function closeSearch(): void {
  searchEl.hidden = true;
  activeTab()?.term.focus();
}
searchInput.addEventListener("input", () => {
  const t = activeTab();
  if (t && searchInput.value) t.search.findNext(searchInput.value, { incremental: true });
});
searchInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    runSearch(!e.shiftKey);
  } else if (e.key === "Escape") {
    e.preventDefault();
    closeSearch();
  }
});
document.getElementById("search-next")!.addEventListener("click", () => runSearch(true));
document.getElementById("search-prev")!.addEventListener("click", () => runSearch(false));
document.getElementById("search-close")!.addEventListener("click", closeSearch);

// ── Copy / paste ─────────────────────────────────────────────────────────────
function doCopy(): void {
  const sel = activeTab()?.term.getSelection();
  if (sel) void navigator.clipboard.writeText(sel);
}

// A promise-based confirmation modal rendered inside the webview. We do NOT use
// window.confirm(): in the Tauri WebKitGTK webview it silently returns true without
// showing a dialog, so it can't gate anything.
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
      activeTab()?.term.focus();
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

// Paste-safety (DESIGN.md §8.3, §13): intercept the native paste event (Ctrl-Shift-V,
// middle-click, Shift-Insert) in the capture phase and gate multi-line pastes.
let pasting = false;
async function handlePaste(text: string): Promise<void> {
  const t = activeTab();
  if (!text || pasting || !t) return;
  const lines = text.split("\n").length;
  if (lines > 1) {
    const ok = await confirmModal(
      `Paste ${lines} lines? A pasted newline will run the command.`,
    );
    if (!ok) return;
  }
  pasting = true;
  t.term.paste(text);
  pasting = false;
}
contentEl.addEventListener(
  "paste",
  (e: ClipboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    void handlePaste(e.clipboardData?.getData("text") ?? "");
  },
  true,
);

// ── Keybinding dispatch ──────────────────────────────────────────────────────
let bindings: Array<[Chord, () => void]> = [];
function rebuildBindings(): void {
  const kb = currentCfg.keybindings;
  bindings = [
    [parseChord(kb.new_tab), () => void createTab()],
    [parseChord(kb.close_tab), () => active >= 0 && closeTab(active)],
    [parseChord(kb.next_tab), () => switchTab(1)],
    [parseChord(kb.prev_tab), () => switchTab(-1)],
    [parseChord(kb.copy), doCopy],
    [parseChord(kb.search), openSearch],
    [parseChord(kb.zoom_in), () => zoom(1)],
    [parseChord(kb.zoom_out), () => zoom(-1)],
    [parseChord(kb.zoom_reset), () => zoom(null)],
  ];
}
rebuildBindings();

document.addEventListener(
  "keydown",
  (e) => {
    // Don't hijack chords while typing in the search box (Esc/Enter handled there).
    if (document.activeElement === searchInput) return;
    for (const [chord, fn] of bindings) {
      if (chordMatches(chord, e)) {
        e.preventDefault();
        e.stopPropagation();
        fn();
        return;
      }
    }
  },
  true, // capture: run before xterm's key handling
);

// ── Window lifecycle ─────────────────────────────────────────────────────────
new ResizeObserver(() => {
  const t = activeTab();
  if (t) fitAndReport(t);
}).observe(contentEl);
window.addEventListener("resize", () => {
  const t = activeTab();
  if (t) fitAndReport(t);
});
window.addEventListener("beforeunload", () => {
  for (const t of tabs) void invoke("close_session", { session: t.id });
});

// First tab.
await createTab();
