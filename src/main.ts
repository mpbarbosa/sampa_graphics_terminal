// Frontend (Layers 3–4): renders terminals with xterm.js and bridges them to the
// Rust cores over Tauri IPC. Deliberately thin — all terminal/session behavior lives
// in the headless crates. M2 adds config-driven theming, tabs, search, and keybinds.

import { Terminal, type ITerminalOptions, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { ImageAddon } from "@xterm/addon-image";
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
  rendering: { gpu: boolean; images: boolean };
  clipboard: { osc52_write: "ask" | "allow" | "deny" };
  features: { palette: boolean; man: boolean; preview: boolean };
  ai: { enabled: boolean; model: string; endpoint: string; send_context: boolean };
  enhance: {
    ps: "off" | "quiet" | "bars" | "inspector";
    min_width: number;
    min_width_bars: number;
    min_width_inspector: number;
  };
}

// The decorated `ps` model returned by the `decorate_ps` bridge command.
interface PsRow {
  pid: string;
  user: string;
  cpu: string;
  mem: string;
  rss: string;
  rss_kb: number;
  start: string;
  command: string;
  cpu_val: number;
  mem_val: number;
}
interface PsBar {
  cpu: string;
  mem: string;
}
interface PsGroup {
  group: string; // dev | browser | desktop | system | shell | other
  rows: number[]; // indices into PsDecorated.rows
  count: number;
  cpu: number;
  rss_kb: number;
}
interface PsDecorated {
  level: "quiet" | "bars" | "inspector";
  columns: string[];
  rows: PsRow[];
  kernel_count: number;
  kernel_summary: string | null;
  // Level 1b (spec §5): signal bars parallel to rows (empty at the quiet level), plus
  // header denominators so a per-core-summed percentage has context.
  bars: PsBar[];
  cpu_total: number;
  mem_total: number;
  core_count: number;
  // Level 1c (spec §6): provenance groups with subtotals (only at the inspector level).
  groups: PsGroup[];
}
// Per-process detail from the ps_enrich query (spec §6 detail pane).
interface PsDetail {
  pid: number;
  ppid: number;
  threads: number;
  state: string;
  state_long: string;
  etimes: number;
}

// Escape-sequence hardening (§13). Terminal output is untrusted: a window/tab title
// set via OSC 0/2 must not carry control characters into our UI.
function sanitizeTitle(t: string): string {
  let out = "";
  for (const ch of t) {
    const code = ch.codePointAt(0)!;
    if (code >= 0x20 && code !== 0x7f) out += ch; // drop C0 controls + DEL
  }
  return out.slice(0, 256);
}

// Open an http(s) link from terminal content: confirm the target (§13), then hand it
// to the core, which re-validates the scheme. Explicit click only — never auto-open.
async function openLink(uri: string): Promise<void> {
  const ok = await confirmModal(`Open this link?\n${uri}`, "Open");
  if (!ok) return;
  try {
    await invoke("open_url", { url: uri });
  } catch (e) {
    console.warn("[sampa] open_url:", e);
  }
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
    // §13: keep title/icon reporting OFF — CSI 20 t / CSI 21 t report the
    // application-settable title back into the PTY, a command-injection vector.
    // The geometry window-ops are safe and useful: size *reports* (CSI 14/16/18 t)
    // only reveal the grid dimensions (like DSR), and grid *resize* (CSI 8 t) is
    // standard xterm behavior that TUIs and the esctest conformance suite rely on.
    windowOptions: {
      getWinTitle: false, // CSI 21 t — denied (injection vector)
      getIconTitle: false, // CSI 20 t — denied (injection vector)
      getWinSizePixels: true, // CSI 14 t — report text-area size in pixels
      getCellSizePixels: true, // CSI 16 t — report cell size in pixels
      getWinSizeChars: true, // CSI 18 t — report text-area size in chars
      // CSI 8 t (grid resize) is intentionally left off: xterm.js intercepts it
      // internally and won't hand it to us to letterbox a fixed grid, so callers
      // (incl. esctest) must resize the window instead. Size reports still work.
    },
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
    Slash: "Slash", // "?" is Shift+Slash on a US layout
    Backspace: "Backspace",
  };
  return named[tok] ?? null;
}

function parseChord(s: string): Chord {
  const chord: Chord = { ctrl: false, shift: false, alt: false, meta: false, code: null };
  if (!s) return chord; // missing/empty binding → never matches (code stays null)
  const parts = s.split("+").map((p) => p.trim());
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
  // What the user has typed on the current command line, reconstructed from keystrokes
  // (for the man panel, preview, and explain). Reset on Enter/Ctrl-C; autosuggestions
  // never enter it since they aren't keystrokes.
  typed: string;
  // Escape-sequence parser state for trackTyped, so arrow keys / function keys don't
  // leak their sequence bodies (e.g. `ESC O B` → "OB") into `typed`. Persists across
  // onData chunks since a sequence can split.
  escState: "none" | "esc" | "csi" | "ss3" | "osc";
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
newTabBtn.addEventListener("click", () => void newTab());

// New tab inheriting the active tab's cwd (from OSC 7 / /proc).
async function newTab(): Promise<void> {
  const t = activeTab();
  const cwd = t
    ? ((await invoke<string | null>("get_session_cwd", { session: t.id })) ?? undefined)
    : undefined;
  await createTab({ cwd });
}
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
  void updateMan(); // reflect the newly-active tab's command line
  void updatePreview();
}

function switchTab(delta: number): void {
  if (tabs.length > 1) activate((active + delta + tabs.length) % tabs.length);
}

function refreshChrome(): void {
  // Hide the tab bar entirely when there's a single tab (chrome-free like M1).
  appEl.classList.toggle("single-tab", tabs.length <= 1);
}

interface ExitPayload {
  code: number;
  success: boolean;
  detail: string;
}

async function createTab(
  opts: { hold?: boolean; title?: string; cwd?: string } = {},
): Promise<void> {
  const pane = document.createElement("div");
  pane.className = "term-pane";
  contentEl.append(pane);

  const term = new Terminal(xtermOptions(currentCfg));
  const fit = new FitAddon();
  const search = new SearchAddon();
  term.loadAddon(fit);
  term.loadAddon(search);

  // Inline images (sixel / iTerm), capped so a hostile stream can't OOM us (§13).
  if (currentCfg.rendering.images) {
    term.loadAddon(new ImageAddon({ sixelSupport: true, storageLimit: 50, pixelLimit: 16_000_000 }));
  }
  // Clickable URLs: plain-text (web-links) and OSC 8. Both route through openLink,
  // which confirms and never auto-opens.
  term.loadAddon(new WebLinksAddon((_event, uri) => void openLink(uri)));
  term.options.linkHandler = { activate: (_event, uri) => void openLink(uri) };

  // OSC 52 clipboard hardening (§13): a *write* is gated by config (ask/allow/deny);
  // a *read* query (`?`) is always denied — never answered — so terminal output can't
  // exfiltrate the clipboard. Returning true marks the sequence handled.
  term.parser.registerOscHandler(52, (data) => {
    const semi = data.indexOf(";");
    const payload = semi >= 0 ? data.slice(semi + 1) : "";
    const policy = currentCfg.clipboard.osc52_write;
    if (payload === "?" || policy === "deny") return true;
    let text = "";
    try {
      text = atob(payload);
    } catch {
      return true; // malformed base64
    }
    if (!text || text.length > 100_000) return true; // empty / oversized
    if (policy === "allow") {
      void navigator.clipboard.writeText(text);
      return true;
    }
    const preview = sanitizeTitle(text.length > 60 ? text.slice(0, 60) + "…" : text);
    void confirmModal(
      `An application wants to copy ${text.length} byte(s) to your clipboard:\n\n${preview}`,
      "Copy",
    ).then((ok) => {
      if (ok) void navigator.clipboard.writeText(text);
    });
    return true;
  });

  // §13 defense-in-depth: never answer a window/icon *title report* (CSI 20 t /
  // CSI 21 t). The title is application-settable, so reporting it back echoes
  // attacker-controlled bytes into stdin — a command-injection vector. Swallow
  // just those two ops (return true = handled, no reply); every other CSI t
  // window-op falls through to xterm, where they stay gated off via windowOptions.
  // DA (CSI c), DSR (CSI n) and DECRQSS (DCS $q) are left to xterm, which answers
  // with fixed / cursor-derived values only — never attacker input — so apps that
  // need capability + cursor reports keep working.
  term.parser.registerCsiHandler({ final: "t" }, (params) => {
    const op = Array.isArray(params[0]) ? params[0][0] : params[0];
    return op === 20 || op === 21;
  });

  term.open(pane);
  pane.style.padding = `${currentCfg.window.padding_y}px ${currentCfg.window.padding_x}px`;

  // GPU renderer (after open, needs the canvas). Fall back to the DOM/canvas renderer
  // if the WebGL context is lost or unavailable.
  if (currentCfg.rendering.gpu) {
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch (e) {
      console.warn("[sampa] WebGL unavailable, using canvas renderer:", e);
    }
  }
  fit.fit();

  const id = await invoke<number>("spawn_session", {
    cols: term.cols,
    rows: term.rows,
    cwd: opts.cwd ?? null,
  });

  // Conformance: DECRQCRA — "request checksum of rectangular area"
  // (CSI Pid ; Pp ; Pt ; Pl ; Pb ; Pr * y). Reply with a 16-bit sum of the cell
  // codepoints in the rectangle so screen-reading conformance tooling (esctest)
  // can verify rendered contents. Empty cells count as space (0x20), matching
  // xterm; reply is DCS Pid ! ~ XXXX ST. Registered here (not before term.open)
  // because it needs the session `id` to write the reply.
  term.parser.registerCsiHandler({ intermediates: "*", final: "y" }, (params) => {
    const at = (i: number, dflt: number) => {
      const v = params[i];
      return typeof v === "number" && v > 0 ? v : dflt;
    };
    const pid = typeof params[0] === "number" && params[0] > 0 ? params[0] : 0;
    const buf = term.buffer.active;
    const top = at(2, 1);
    const left = at(3, 1);
    const bottom = at(4, term.rows);
    const right = at(5, term.cols);
    let sum = 0;
    for (let row = top; row <= bottom; row++) {
      const line = buf.getLine(buf.baseY + row - 1);
      if (!line) continue;
      for (let col = left; col <= right; col++) {
        const code = line.getCell(col - 1)?.getCode() ?? 0;
        sum += code === 0 ? 0x20 : code;
      }
    }
    const hex = (sum & 0xffff).toString(16).toUpperCase().padStart(4, "0");
    void invoke("write_session", {
      session: id,
      data: bytesToB64(encoder.encode(`\x1bP${pid}!~${hex}\x1b\\`)),
    });
    return true;
  });

  const unlisten: UnlistenFn[] = [];
  unlisten.push(
    await listen<string>(`pty://output/${id}`, (e) => term.write(b64ToBytes(e.payload))),
  );
  // On exit, close the tab — unless --hold, which keeps it open showing the status.
  unlisten.push(
    await listen<ExitPayload>(`pty://exit/${id}`, (e) => {
      if (opts.hold) {
        const { code, success, detail } = e.payload;
        const color = success ? "32" : "31"; // green / red
        term.write(
          `\r\n\x1b[${color}m[${detail || `exited (code ${code})`} — Ctrl+Shift+W to close]\x1b[0m\r\n`,
        );
      } else {
        closeTabById(id);
      }
    }),
  );

  term.onData((data) => {
    void invoke("write_session", { session: id, data: bytesToB64(encoder.encode(data)) });
    trackTyped(tab, data);
    scheduleMan(); // refresh the man panel as the command line changes
    if (data.includes("\r") || data.includes("\n")) {
      hidePreview(); // submitted — the real output is in the terminal now
    } else {
      schedulePreview();
    }
  });
  term.onBell(() => {
    if (currentCfg.bell.visual) flash(pane);
  });

  // Listeners are attached — release the backend's output pump (avoids losing the
  // output of a fast `-e` command that exits before we're listening).
  await invoke("session_ready", { session: id });

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

  const tab: Tab = { id, term, fit, search, pane, tabEl, titleEl, unlisten, typed: "", escState: "none" };

  if (opts.title) {
    // An explicit --title wins; don't let the shell's OSC title override it.
    titleEl.textContent = opts.title;
    tabEl.title = opts.title;
  } else {
    term.onTitleChange((t) => {
      const clean = sanitizeTitle(t);
      titleEl.textContent = clean || "shell";
      tabEl.title = clean;
    });
  }
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
  // Re-sync the M4 feature toggles to config (runtime Ctrl+Shift+M/R are transient).
  manEnabled = currentCfg.features.man;
  if (manEnabled) void updateMan();
  else hideMan();
  previewEnabled = currentCfg.features.preview;
  if (previewEnabled) void updatePreview();
  else hidePreview();
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

// ── Command palette (DESIGN.md §10.1) ────────────────────────────────────────
// Fuzzy over $PATH executables; Enter INSERTS "<cmd> " at the prompt — it never
// auto-runs. The command list comes from the core (list_commands), cached here.
const paletteEl = document.getElementById("palette")!;
const paletteInput = document.getElementById("palette-input") as HTMLInputElement;
const paletteList = document.getElementById("palette-list")!;
const PALETTE_MAX = 60;

let allCommands: string[] | null = null;
let paletteResults: string[] = [];
let paletteSelected = 0;

// Flexible matcher (both args lowercased). Returns a score (higher is better) and
// the set of matched character indices for highlighting, or null if no match.
//
// Tiers, strongest first, so a contiguous hit always outranks a scattered one:
//   exact  ▸  prefix  ▸  word-boundary substring (git-`grep`)  ▸  substring
//   anywhere (e`grep`)  ▸  loose subsequence (d-c-p → `d`o`c`ker-com`p`ose).
// This is why typing "grep" clusters the whole grep family at the top while
// scattered noise like "gv2ray-proxy-helper" sinks to the bottom.
//
// A space splits the query into tokens that must each match (AND), so "git grep"
// or "doc comp" work — each token is scored independently and the hits merge.
type Match = { score: number; hits: Set<number> };

const WORD_BOUNDARY = new Set(["-", "_", ".", "/", "@", "+"]);

function rangeSet(start: number, len: number): Set<number> {
  const s = new Set<number>();
  for (let i = 0; i < len; i++) s.add(start + i);
  return s;
}

// Score a single (space-free) token against cmd.
function scoreToken(cmd: string, t: string): Match | null {
  if (cmd === t) return { score: 1000, hits: rangeSet(0, t.length) };
  const sub = cmd.indexOf(t);
  if (sub !== -1) {
    let s = 200 - Math.min(sub, 100); // earlier occurrence is better
    if (sub === 0) s += 100; // prefix
    else if (WORD_BOUNDARY.has(cmd[sub - 1])) s += 60; // token start (git-grep)
    return { score: s, hits: rangeSet(sub, t.length) };
  }
  // Loose subsequence fallback (kept below every substring hit).
  let ci = 0;
  let score = 0;
  let prev = -2;
  const hits = new Set<number>();
  for (const ch of t) {
    const idx = cmd.indexOf(ch, ci);
    if (idx === -1) return null;
    score -= idx - ci; // gap penalty
    if (idx === prev + 1) score += 5; // contiguity bonus
    if (idx === 0) score += 10; // prefix bonus
    hits.add(idx);
    prev = idx;
    ci = idx + 1;
  }
  return { score, hits };
}

function scoreMatch(cmd: string, q: string): Match | null {
  if (!q) return { score: 0, hits: new Set() };
  const hits = new Set<number>();
  let score = 0;
  for (const t of q.split(/\s+/).filter(Boolean)) {
    const m = scoreToken(cmd, t);
    if (!m) return null; // every token must match
    score += m.score;
    m.hits.forEach((i) => hits.add(i));
  }
  return { score: score - cmd.length * 0.1, hits }; // mild shorter-name preference
}

function renderPalette(): void {
  paletteList.textContent = "";
  if (paletteResults.length === 0) {
    const empty = document.createElement("li");
    empty.id = "palette-empty";
    empty.textContent = allCommands === null ? "Loading…" : "No matching command";
    paletteList.append(empty);
    return;
  }
  const q = paletteInput.value.toLowerCase();
  paletteResults.forEach((cmd, i) => {
    const li = document.createElement("li");
    li.setAttribute("role", "option");
    if (i === paletteSelected) li.classList.add("selected");
    highlightInto(li, cmd, q);
    li.addEventListener("mousedown", (e) => {
      e.preventDefault();
      insertCommand(cmd);
    });
    paletteList.append(li);
  });
  paletteList.children[paletteSelected]?.scrollIntoView({ block: "nearest" });
}

// Render `cmd` with the matched characters wrapped in <span class="match">, using
// the exact hit set the matcher chose (so a substring hit highlights the run).
function highlightInto(li: HTMLElement, cmd: string, q: string): void {
  if (!q) {
    li.textContent = cmd;
    return;
  }
  const hits = scoreMatch(cmd.toLowerCase(), q)?.hits ?? new Set<number>();
  for (let i = 0; i < cmd.length; i++) {
    const span = document.createElement("span");
    if (hits.has(i)) span.className = "match";
    span.textContent = cmd[i];
    li.append(span);
  }
}

function filterPalette(): void {
  const q = paletteInput.value.toLowerCase();
  const cmds = allCommands ?? [];
  paletteResults = cmds
    .map((c) => [c, scoreMatch(c.toLowerCase(), q)] as const)
    .filter((x): x is readonly [string, Match] => x[1] !== null)
    .sort((a, b) => b[1].score - a[1].score)
    .slice(0, PALETTE_MAX)
    .map((x) => x[0]);
  paletteSelected = 0;
  renderPalette();
}

async function openPalette(): Promise<void> {
  paletteEl.hidden = false;
  paletteInput.value = "";
  paletteInput.focus();
  if (allCommands === null) {
    renderPalette(); // shows "Loading…"
    allCommands = await invoke<string[]>("list_commands");
  }
  filterPalette();
}
function closePalette(): void {
  paletteEl.hidden = true;
  activeTab()?.term.focus();
}
function insertCommand(cmd: string): void {
  const t = activeTab();
  closePalette();
  if (!t) return;
  // Write the command + a space to the shell's line editor — no newline, so it is
  // inserted for the user to edit/run, never auto-executed (DESIGN.md §10.1).
  void invoke("write_session", {
    session: t.id,
    data: bytesToB64(encoder.encode(cmd + " ")),
  });
}

paletteInput.addEventListener("input", filterPalette);
paletteInput.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    if (paletteResults.length) {
      paletteSelected = (paletteSelected + 1) % paletteResults.length;
      renderPalette();
    }
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (paletteResults.length) {
      paletteSelected = (paletteSelected - 1 + paletteResults.length) % paletteResults.length;
      renderPalette();
    }
  } else if (e.key === "Enter") {
    e.preventDefault();
    const cmd = paletteResults[paletteSelected];
    if (cmd) insertCommand(cmd);
  } else if (e.key === "Escape") {
    e.preventDefault();
    closePalette();
  }
});
paletteEl.addEventListener("mousedown", (e) => {
  if (e.target === paletteEl) closePalette(); // click backdrop to dismiss
});

// ── Keyboard-shortcut help overlay (Ctrl+Shift+?) ────────────────────────────
// Built from the *live* keybinding config so it always reflects the user's binds.
const helpEl = document.getElementById("help")!;
const helpList = document.getElementById("help-list")!;

// The configurable actions, in display order: [config key, human label].
const HELP_ACTIONS: Array<[string, string]> = [
  ["new_tab", "New tab"],
  ["close_tab", "Close tab"],
  ["next_tab", "Next tab"],
  ["prev_tab", "Previous tab"],
  ["copy", "Copy selection"],
  ["search", "Find in terminal"],
  ["palette", "Command palette"],
  ["toggle_man", "Toggle man-page panel"],
  ["toggle_preview", "Toggle command preview"],
  ["enhance_ps", "Enhance last ps output"],
  ["explain", "Explain typed command (AI)"],
  ["zoom_in", "Zoom in"],
  ["zoom_out", "Zoom out"],
  ["zoom_reset", "Reset zoom"],
  ["help", "This help"],
];
// Built-in shortcuts that aren't config keybindings but are worth documenting.
const HELP_FIXED: Array<[string, string]> = [
  ["Ctrl+Shift+V", "Paste (multi-line pastes ask first)"],
  ["Esc", "Close this help, an overlay, or a panel"],
];

// Turn a config chord ("Ctrl+Shift+Slash") into something readable ("Ctrl+Shift+?").
const CHORD_SYMBOLS: Record<string, string> = {
  Slash: "?",
  Equal: "=",
  Plus: "+",
  Minus: "−",
  Right: "→",
  Left: "←",
  Up: "↑",
  Down: "↓",
};
function prettyChord(s: string): string {
  return s
    .split("+")
    .map((p) => CHORD_SYMBOLS[p.trim()] ?? p.trim())
    .join("+");
}

function openHelp(): void {
  const kb = currentCfg.keybindings;
  helpList.textContent = "";
  const rows: Array<[string, string]> = [
    ...HELP_ACTIONS.map(([key, label]) => [prettyChord(kb[key] ?? ""), label] as [string, string]),
    ...HELP_FIXED,
  ];
  for (const [chord, label] of rows) {
    const li = document.createElement("li");
    const k = document.createElement("kbd");
    k.textContent = chord;
    const d = document.createElement("span");
    d.className = "help-desc";
    d.textContent = label;
    li.append(k, d);
    helpList.append(li);
  }
  helpEl.hidden = false;
}
function closeHelp(): void {
  helpEl.hidden = true;
  activeTab()?.term.focus();
}
function toggleHelp(): void {
  if (helpEl.hidden) openHelp();
  else closeHelp();
}
helpEl.addEventListener("mousedown", (e) => {
  if (e.target === helpEl) closeHelp(); // click backdrop to dismiss
});
document.getElementById("help-close")!.addEventListener("click", closeHelp);
// Esc closes help (it has no focused input, so handle it here).
document.addEventListener(
  "keydown",
  (e) => {
    if (!helpEl.hidden && e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeHelp();
    }
  },
  true,
);

// ── Claude-API command suggester (Ctrl+Shift+A, opt-in §13) ──────────────────
// Turns a natural-language request into a command via one Messages API call. The
// overlay makes the network egress explicit; the result is *inserted* at the
// prompt, never auto-run (the same boundary as the palette). Context is attached
// only if the user opted into `[ai] send_context`.
const aiEl = document.getElementById("ai")!;
const aiInput = document.getElementById("ai-input") as HTMLInputElement;
const aiNote = document.getElementById("ai-note")!;
const aiResult = document.getElementById("ai-result")!;
const aiCommand = document.getElementById("ai-command")!;
const aiExplanation = document.getElementById("ai-explanation")!;
const aiStatus = document.getElementById("ai-status")!;
let aiSuggestion = "";

function resetAiOverlay(): void {
  aiResult.hidden = true;
  aiStatus.hidden = true;
  aiNote.hidden = false;
  aiSuggestion = "";
}
function openAi(): void {
  resetAiOverlay();
  aiInput.value = "";
  aiEl.hidden = false;
  aiInput.focus();
  if (!currentCfg.ai.enabled) {
    aiNote.hidden = true;
    aiStatus.hidden = false;
    aiStatus.textContent = "The AI suggester is off. Set [ai] enabled = true in config.toml.";
  }
}
function closeAi(): void {
  aiEl.hidden = true;
  activeTab()?.term.focus();
}

async function askAi(): Promise<void> {
  const prompt = aiInput.value.trim();
  if (!prompt || !currentCfg.ai.enabled) return;
  aiNote.hidden = true;
  aiResult.hidden = true;
  aiStatus.hidden = false;
  aiStatus.textContent = "Asking Claude…";
  // Only gather context when the user opted in; the backend re-checks too.
  let context: string | null = null;
  if (currentCfg.ai.send_context) {
    const t = activeTab();
    if (t) {
      const buf = t.term.buffer.active;
      const lines: string[] = [];
      const start = Math.max(0, buf.length - 40);
      for (let y = start; y < buf.length; y++) lines.push(buf.getLine(y)?.translateToString(true) ?? "");
      context = lines.join("\n").trimEnd();
    }
  }
  try {
    const s = await invoke<{ command: string; explanation: string }>("suggest_command", {
      prompt,
      context,
    });
    aiSuggestion = s.command;
    aiCommand.textContent = s.command;
    aiExplanation.textContent = s.explanation;
    aiStatus.hidden = true;
    aiResult.hidden = false;
  } catch (err) {
    aiStatus.textContent = `Couldn't get a suggestion: ${String(err)}`;
  }
}

aiInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    // Enter runs the query while typing, or inserts once a suggestion is shown.
    if (!aiResult.hidden && aiSuggestion) {
      insertCommand(aiSuggestion);
      closeAi();
    } else {
      void askAi();
    }
  } else if (e.key === "Escape") {
    e.preventDefault();
    closeAi();
  }
});
document.getElementById("ai-insert")!.addEventListener("click", () => {
  if (aiSuggestion) {
    insertCommand(aiSuggestion);
    closeAi();
  }
});
document.getElementById("ai-cancel")!.addEventListener("click", closeAi);
aiEl.addEventListener("mousedown", (e) => {
  if (e.target === aiEl) closeAi(); // click backdrop to dismiss
});

// ── Command explainer popup (Ctrl+Shift+X / [ai]) ────────────────────────────
// The inverse of the suggester: send the command line the user has typed to the Claude
// API and show a plain-prose description in a read-only popup. Pressing the shortcut is
// the deliberate send (the command line leaves the machine); the gate/key live in the
// core (explain_command), and nothing is ever executed.
const explainEl = document.getElementById("explain")!;
const explainCmd = document.getElementById("explain-cmd")!;
const explainBody = document.getElementById("explain-body")!;
let explainSeq = 0; // guards against out-of-order async results

function closeExplain(): void {
  if (explainEl.hidden) return;
  explainEl.hidden = true;
  activeTab()?.term.focus();
}

function setExplainBody(text: string, muted: boolean): void {
  explainBody.textContent = text; // model/error text — textContent, never innerHTML
  explainBody.classList.toggle("explain-muted", muted);
}

async function explainCurrent(): Promise<void> {
  const command = activeTab()?.typed.trim() ?? "";
  if (!command) return; // nothing typed to describe
  explainCmd.textContent = command;
  setExplainBody("Asking Claude…", true);
  explainEl.hidden = false;
  const seq = ++explainSeq;
  try {
    const desc = await invoke<string>("explain_command", { command });
    if (seq !== explainSeq || explainEl.hidden) return; // superseded / dismissed
    setExplainBody(desc, false);
  } catch (e) {
    if (seq !== explainSeq || explainEl.hidden) return;
    setExplainBody(String(e), true); // e.g. "AI is disabled…" / "ANTHROPIC_API_KEY is not set"
  }
}

document.getElementById("explain-close")!.addEventListener("click", closeExplain);
explainEl.addEventListener("mousedown", (e) => {
  if (e.target === explainEl) closeExplain(); // click backdrop to dismiss
});
// The popup has no input of its own, so close it on Esc from the capture phase (before
// the terminal or the global chord dispatcher see the key).
window.addEventListener(
  "keydown",
  (e) => {
    if (!explainEl.hidden && e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeExplain();
    }
  },
  true,
);

// ── Live man panel (DESIGN.md §10.2) ─────────────────────────────────────────
// As you type a command, show `man <cmd>` beside the terminal. Robust because the
// command boundary comes from the OSC 133 B mark (needs the shell integration hook);
// gated on the token being a real $PATH command, so it collapses for keywords and
// unknown words rather than showing a stale page.
const manPanel = document.getElementById("manpanel")!;
const manTitle = document.getElementById("man-title")!;
const manBody = document.getElementById("man-body")!;

let manEnabled = currentCfg.features.man;
let commandSet: Set<string> | null = null;
let manCurrent = ""; // command whose page is currently shown
let manTimer: ReturnType<typeof setTimeout> | undefined;

async function ensureCommandSet(): Promise<Set<string>> {
  if (!commandSet) commandSet = new Set(await invoke<string[]>("list_commands"));
  return commandSet;
}

function refitActive(): void {
  const t = activeTab();
  if (t) fitAndReport(t);
}

function hideMan(): void {
  if (manPanel.hidden) return;
  manPanel.hidden = true;
  manCurrent = "";
  refitActive();
}

async function showMan(cmd: string): Promise<void> {
  if (cmd === manCurrent && !manPanel.hidden) return;
  const text = await invoke<string | null>("render_man", { cmd });
  if (!manEnabled) return; // toggled off while awaiting
  if (text) {
    manTitle.textContent = `man ${cmd}`;
    manBody.textContent = text;
    manBody.scrollTop = 0;
    manCurrent = cmd;
    if (manPanel.hidden) {
      manPanel.hidden = false;
      refitActive();
    }
  } else {
    hideMan();
  }
}

// Reconstruct the current command line from a chunk of keystrokes. This tracks what
// the user typed (not what the shell renders), so prompts and autosuggestions never
// interfere. Handles Enter/Ctrl-C (reset) and Backspace; other control sequences
// (arrows, history) are ignored — a best-effort that's right for typed-in commands.
function trackTyped(tab: Tab, data: string): void {
  for (const ch of data) {
    const code = ch.codePointAt(0)!;
    // Consume escape sequences whole, so arrow/function keys (e.g. `ESC O B` for the
    // down arrow, `ESC [ C` for right) don't leak their bodies ("OB", "[C") into `typed`.
    switch (tab.escState) {
      case "esc": // just after ESC: classify the sequence introducer
        tab.escState =
          ch === "[" ? "csi" : ch === "O" ? "ss3" : ch === "]" ? "osc" : "none";
        continue;
      case "csi": // ESC [ … ends at a final byte 0x40–0x7e
        if (code >= 0x40 && code <= 0x7e) tab.escState = "none";
        continue;
      case "ss3": // ESC O <one final byte> (application-mode cursor/function keys)
        tab.escState = "none";
        continue;
      case "osc": // ESC ] … ends at BEL, or ST (ESC \)
        if (code === 0x07) tab.escState = "none";
        else if (code === 0x1b) tab.escState = "esc";
        continue;
    }
    // Normal state.
    if (code === 0x1b) tab.escState = "esc"; // start of an escape sequence
    else if (ch === "\r" || ch === "\n" || code === 0x03) tab.typed = ""; // submit / Ctrl-C
    else if (code === 0x7f || code === 0x08) tab.typed = tab.typed.slice(0, -1); // backspace
    else if (code >= 0x20) tab.typed += ch; // printable
  }
}

// The first token of what's typed, if it's a real $PATH command.
function detectCommand(cmds: Set<string>): string | null {
  const typed = activeTab()?.typed.trimStart() ?? "";
  const tok = typed.split(/\s+/)[0] ?? "";
  return tok && cmds.has(tok) ? tok : null;
}

async function updateMan(): Promise<void> {
  if (!manEnabled) return;
  const cmds = await ensureCommandSet();
  const token = detectCommand(cmds);
  if (token) void showMan(token);
  else hideMan();
}

function scheduleMan(): void {
  if (!manEnabled) return;
  clearTimeout(manTimer);
  manTimer = setTimeout(() => void updateMan(), 300);
}

function toggleMan(): void {
  manEnabled = !manEnabled;
  if (manEnabled) void updateMan();
  else hideMan();
}
document.getElementById("man-close")!.addEventListener("click", () => {
  manEnabled = false;
  hideMan();
});

// ── Safe auto-run preview (DESIGN.md §10.3) ──────────────────────────────────
// As you type a syntactically valid, read-only command, run it in a throwaway shell
// (in the session's cwd) and show the output below the terminal. The gate is
// authoritative in the core (render_preview); the frontend only displays the result.
const previewPanel = document.getElementById("preview")!;
const previewTitle = document.getElementById("preview-title")!;
const previewBody = document.getElementById("preview-body")!;

let previewEnabled = currentCfg.features.preview;
let previewTimer: ReturnType<typeof setTimeout> | undefined;
let previewSeq = 0; // guards against out-of-order async results

function hidePreview(): void {
  if (previewPanel.hidden) return;
  previewPanel.hidden = true;
  refitActive();
}

async function updatePreview(): Promise<void> {
  if (!previewEnabled) return;
  const t = activeTab();
  const line = t?.typed.trim() ?? "";
  if (!t || !line) {
    hidePreview();
    return;
  }
  const seq = ++previewSeq;
  const output = await invoke<string | null>("render_preview", { session: t.id, line });
  if (seq !== previewSeq || !previewEnabled) return; // superseded / toggled off
  if (output !== null) {
    previewTitle.textContent = `Preview: ${line}`;
    previewBody.textContent = output || "(no output)";
    previewBody.scrollTop = 0;
    if (previewPanel.hidden) {
      previewPanel.hidden = false;
      refitActive();
    }
  } else {
    hidePreview();
  }
}

function schedulePreview(): void {
  if (!previewEnabled) return;
  clearTimeout(previewTimer);
  previewTimer = setTimeout(() => void updatePreview(), 550);
}

function togglePreview(): void {
  previewEnabled = !previewEnabled;
  if (previewEnabled) void updatePreview();
  else hidePreview();
}
document.getElementById("preview-close")!.addEventListener("click", () => {
  previewEnabled = false;
  hidePreview();
});

// ── ps(1) output enhancement panel (Ctrl+Shift+E / [enhance] ps) ──────────────
// Manual trigger: after running `ps aux`, press the key to decorate the most recent
// ps table (scraped from the terminal buffer) into a panel — dim zeros, size units,
// kernel threads folded. The raw output stays byte-identical in the scrollback; the
// gate + decoration are authoritative in the core (decorate_ps). Levels bars/inspector
// reuse the same model; only 1a is rendered today.
const psPanel = document.getElementById("pspanel")!;
const psTitle = document.getElementById("pspanel-title")!;
const psThead = document.querySelector("#pspanel-table thead")!;
const psTbody = document.querySelector("#pspanel-table tbody")!;
const psFold = document.getElementById("pspanel-fold")!;
const psBody = document.getElementById("pspanel-body")!;
const psDetail = document.getElementById("pspanel-detail")!;

// The decorated model currently shown, plus the live sort key (spec §5 — c/m/p re-sort
// without re-running the command). null ⇒ ps's native order.
let psModel: PsDecorated | null = null;
type PsSort = "cpu" | "mem" | "pid";
let psSort: PsSort | null = null;

// Level 1c inspector state (spec §6): the selected row (index into psModel.rows), the
// collapsed provenance groups, the visible row order for ↑↓, and an async guard for the
// per-row ps_enrich detail fetch. The tr elements are kept so selection re-highlights
// without a full repaint.
let psSelected: number | null = null;
const psCollapsed = new Set<string>();
let psVisibleOrder: number[] = [];
const psRowEls = new Map<number, HTMLElement>();
let psDetailSeq = 0;

const isInspector = (): boolean => psModel?.level === "inspector";

// Human-readable process group name for a header row.
const PS_GROUP_LABEL: Record<string, string> = {
  dev: "Dev",
  browser: "Browser",
  desktop: "Desktop",
  system: "System",
  shell: "Shell",
  other: "Other",
};

function fmtElapsed(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${secs % 60}s`;
  return `${secs}s`;
}
function fmtKb(kb: number): string {
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(1)}G`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)}M`;
  return `${kb}K`;
}

// The exact `ps aux` header, used to locate the most recent table in the scrollback.
const PS_AUX_HEADER = [
  "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME", "COMMAND",
];
// Columns whose cells read left-to-right; the numeric columns stay right-aligned.
const PS_LEFT = new Set(["PID", "USER", "START", "COMMAND"]);

function hidePs(): void {
  if (psPanel.hidden) return;
  psPanel.hidden = true;
  psModel = null;
  psSelected = null;
  psCollapsed.clear();
  psDetail.hidden = true;
  psDetail.replaceChildren();
  refitActive();
}

// Reorder a decorated model by the live sort key, keeping the parallel bars[] in lockstep.
// Returns a shallow copy; the original (ps-native order) is preserved in psModel.
function sortedPsModel(m: PsDecorated, key: PsSort): PsDecorated {
  const idx = m.rows.map((_, i) => i);
  const cmp =
    key === "pid"
      ? (a: number, b: number) => Number(m.rows[a].pid) - Number(m.rows[b].pid)
      : key === "mem"
        ? (a: number, b: number) => m.rows[b].rss_kb - m.rows[a].rss_kb
        : (a: number, b: number) => m.rows[b].cpu_val - m.rows[a].cpu_val;
  idx.sort(cmp);
  return {
    ...m,
    rows: idx.map((i) => m.rows[i]),
    bars: m.bars.length ? idx.map((i) => m.bars[i]) : [],
  };
}

// Apply the current sort (if any) and repaint, refreshing the title's hint. The inspector
// (level 1c) renders grouped + selectable rather than the flat sortable table.
function repaintPs(): void {
  if (!psModel) return;
  if (isInspector()) {
    renderInspector(psModel);
    psTitle.textContent = `${psTitleBase}  (↑↓ select · k kill · y/Y copy · ←→ fold)`;
    return;
  }
  const view = psSort ? sortedPsModel(psModel, psSort) : psModel;
  renderPs(view);
  const sortNote = psSort ? ` · sorted by ${psSort}` : "";
  psTitle.textContent = `${psTitleBase}${sortNote}  (c/m/p sort)`;
}

let psTitleBase = "Processes";

// The inspector's compact column set (spec §6) — the detail pane carries the rest.
const PS_INSPECTOR_COLS = ["PID", "%CPU", "%MEM", "RSS", "COMMAND"];

// Render the two-pane inspector: rows grouped by provenance with subtotals (spec §6),
// each row selectable; the detail pane (right) shows the selected process.
function renderInspector(model: PsDecorated): void {
  psDetail.hidden = false;
  psThead.replaceChildren();
  const htr = document.createElement("tr");
  for (const col of PS_INSPECTOR_COLS) {
    const th = document.createElement("th");
    th.textContent = col;
    if (col === "PID") th.className = "ps-l";
    if (col === "COMMAND") th.className = "ps-cmd";
    htr.appendChild(th);
  }
  psThead.appendChild(htr);

  psTbody.replaceChildren();
  psRowEls.clear();
  psVisibleOrder = [];

  for (const g of model.groups) {
    const collapsed = psCollapsed.has(g.group);
    // Group header row — clickable to collapse/expand.
    const gtr = document.createElement("tr");
    gtr.className = "ps-group";
    const gtd = document.createElement("td");
    gtd.colSpan = PS_INSPECTOR_COLS.length;
    const name = PS_GROUP_LABEL[g.group] ?? g.group;
    gtd.append(`${collapsed ? "▸" : "▾"} ${name}  `);
    const sub = document.createElement("span");
    sub.className = "ps-g-sub";
    sub.textContent = `${g.count} · ${g.cpu.toFixed(1)}% cpu · ${fmtKb(g.rss_kb)}`;
    gtd.appendChild(sub);
    gtr.appendChild(gtd);
    gtr.addEventListener("click", () => toggleGroup(g.group));
    psTbody.appendChild(gtr);
    if (collapsed) continue;

    for (const ri of g.rows) {
      const r = model.rows[ri];
      const tr = document.createElement("tr");
      const cell = (text: string, cls?: string) => {
        const td = document.createElement("td");
        td.textContent = text;
        if (cls) td.className = cls;
        tr.appendChild(td);
      };
      cell(r.pid, "ps-l");
      cell(r.cpu, r.cpu === "–" ? "ps-zero" : psBand(r.cpu_val));
      cell(r.mem, r.mem === "–" ? "ps-zero" : psBand(r.mem_val));
      cell(r.rss, r.rss === "–" ? "ps-zero" : undefined);
      const cmd = document.createElement("td");
      cmd.className = "ps-cmd";
      cmd.textContent = r.command;
      cmd.title = r.command;
      tr.appendChild(cmd);
      tr.addEventListener("click", () => selectPs(ri));
      psTbody.appendChild(tr);
      psRowEls.set(ri, tr);
      psVisibleOrder.push(ri);
    }
  }
  psFold.textContent = model.kernel_summary ?? "";

  // Keep a valid selection: the current one if still visible, else the first row.
  if (psSelected === null || !psRowEls.has(psSelected)) {
    psSelected = psVisibleOrder[0] ?? null;
  }
  updatePsSelection();
}

// Highlight the selected row, scroll it into view, and refresh the detail pane.
function updatePsSelection(): void {
  for (const el of psRowEls.values()) el.classList.remove("ps-sel");
  if (psSelected === null) {
    psDetail.replaceChildren();
    return;
  }
  const el = psRowEls.get(psSelected);
  if (el) {
    el.classList.add("ps-sel");
    el.scrollIntoView({ block: "nearest" });
  }
  void fetchPsDetail(psSelected);
}

function selectPs(rowIdx: number): void {
  psSelected = rowIdx;
  updatePsSelection();
  psBody.focus();
}

function movePsSelection(delta: number): void {
  if (!psVisibleOrder.length) return;
  const cur = psSelected === null ? -1 : psVisibleOrder.indexOf(psSelected);
  const next = Math.max(0, Math.min(psVisibleOrder.length - 1, cur + delta));
  psSelected = psVisibleOrder[next];
  updatePsSelection();
}

// Fetch and render the detail pane for the selected process (spec §6 detail pane). The
// enrich query is per-row and async, so a sequence guard drops stale results.
async function fetchPsDetail(rowIdx: number): Promise<void> {
  if (!psModel) return;
  const row = psModel.rows[rowIdx];
  const seq = ++psDetailSeq;
  const details = await invoke<PsDetail[]>("ps_enrich", { pids: [Number(row.pid)] });
  if (seq !== psDetailSeq || psSelected !== rowIdx) return; // superseded
  renderPsDetail(row, details[0]);
}

function renderPsDetail(row: PsRow, d: PsDetail | undefined): void {
  psDetail.replaceChildren();
  const cmd = document.createElement("p");
  cmd.className = "ps-d-cmd";
  cmd.textContent = row.command; // untrusted — textContent, never innerHTML
  psDetail.appendChild(cmd);

  const dl = document.createElement("dl");
  const add = (k: string, v: string) => {
    const dt = document.createElement("dt");
    dt.textContent = k;
    const dd = document.createElement("dd");
    dd.textContent = v;
    dl.append(dt, dd);
  };
  add("pid", row.pid);
  add("user", row.user);
  add("%cpu", row.cpu === "–" ? "0" : row.cpu);
  add("%mem", row.mem === "–" ? "0" : row.mem);
  add("rss", row.rss);
  if (d) {
    add("ppid", String(d.ppid));
    add("threads", String(d.threads));
    add("state", `${d.state_long} (${d.state})`);
    const started = new Date(Date.now() - d.etimes * 1000);
    add("started", `${started.toLocaleString()} (${fmtElapsed(d.etimes)} ago)`);
  }
  psDetail.appendChild(dl);

  const hint = document.createElement("div");
  hint.className = "ps-d-hint";
  hint.textContent = "k → kill at prompt (never run) · y copy pid · Y copy command";
  psDetail.appendChild(hint);
}

function toggleGroup(name: string): void {
  if (psCollapsed.has(name)) psCollapsed.delete(name);
  else psCollapsed.add(name);
  repaintPs();
}

// Collapse/expand the group the selected row belongs to (← / →, h / l).
function foldSelectedGroup(collapse: boolean): void {
  if (psSelected === null || !psModel) return;
  const g = psModel.groups.find((gr) => gr.rows.includes(psSelected!));
  if (!g) return;
  if (collapse) psCollapsed.add(g.group);
  else psCollapsed.delete(g.group);
  repaintPs();
}

// The signal action (spec §6): compose `kill <pid>` at the user's prompt — never executed.
// The user's own shell runs what they type; this keeps the insert-never-run boundary (§13).
function signalSelected(): void {
  if (psSelected === null || !psModel) return;
  const pid = psModel.rows[psSelected].pid;
  const t = activeTab();
  if (!t) return;
  hidePs();
  void invoke("write_session", {
    session: t.id,
    data: bytesToB64(encoder.encode(`kill ${pid} `)),
  });
  t.term.focus();
}

function copySelected(full: boolean): void {
  if (psSelected === null || !psModel) return;
  const row = psModel.rows[psSelected];
  void navigator.clipboard.writeText(full ? row.command : row.pid);
}

// Reconstruct logical lines from the xterm buffer, joining rows xterm marked as wrapped
// so a ps row wider than the terminal isn't split mid-field.
function scrapeLogicalLines(term: Terminal): string[] {
  const buf = term.buffer.active;
  const lines: string[] = [];
  let cur = "";
  let started = false;
  for (let i = 0; i < buf.length; i++) {
    const line = buf.getLine(i);
    if (!line) continue;
    const text = line.translateToString(true);
    if (line.isWrapped) {
      cur += text;
    } else {
      if (started) lines.push(cur);
      cur = text;
      started = true;
    }
  }
  if (started) lines.push(cur);
  return lines;
}

// Slice from the LAST `ps aux` header to the end of the buffer — the most recent run.
// Returns null when no ps table is present.
function lastPsBlock(term: Terminal): string | null {
  const lines = scrapeLogicalLines(term);
  let header = -1;
  for (let i = lines.length - 1; i >= 0; i--) {
    const toks = lines[i].trim().split(/\s+/);
    if (toks.length === PS_AUX_HEADER.length && toks.every((t, j) => t === PS_AUX_HEADER[j])) {
      header = i;
      break;
    }
  }
  return header < 0 ? null : lines.slice(header).join("\n");
}

// Colour band for a %CPU/%MEM value (spec §7) — relative, redundant with position.
function psBand(v: number): string {
  if (v === 0) return "ps-zero";
  if (v < 1) return "ps-low";
  if (v < 5) return "ps-mid";
  if (v < 10) return "ps-high";
  return "ps-crit";
}

function renderPs(model: PsDecorated): void {
  psDetail.hidden = true; // the detail pane belongs to the inspector level only
  // Level 1b (spec §5): bars present ⇒ show magnitude bars + header denominators.
  const bars = model.bars.length > 0;

  // Header row. At the bars level the %CPU/%MEM headers carry the denominators so a
  // per-core-summed percentage has context (e.g. "%CPU 32.7% of 800%").
  psThead.replaceChildren();
  const htr = document.createElement("tr");
  for (const col of model.columns) {
    const th = document.createElement("th");
    let label = col;
    if (bars && col === "%CPU") {
      label = `%CPU ${model.cpu_total.toFixed(1)}% of ${model.core_count * 100}%`;
    } else if (bars && col === "%MEM") {
      label = `%MEM ${model.mem_total.toFixed(1)}%`;
    }
    th.textContent = label;
    if (PS_LEFT.has(col)) th.className = col === "COMMAND" ? "ps-cmd" : "ps-l";
    htr.appendChild(th);
  }
  psThead.appendChild(htr);

  // Data rows. textContent throughout — command text is untrusted, never innerHTML.
  psTbody.replaceChildren();
  model.rows.forEach((r, i) => {
    const tr = document.createElement("tr");
    const cell = (text: string, cls?: string) => {
      const td = document.createElement("td");
      td.textContent = text;
      if (cls) td.className = cls;
      tr.appendChild(td);
      return td;
    };
    // %CPU/%MEM cell: at the bars level, a fixed-width value + the block-glyph bar (in a
    // pre span so its space-padding aligns bars across rows for length comparison).
    const metric = (value: string, band: string, barStr: string | undefined) => {
      if (barStr === undefined) {
        cell(value, band);
        return;
      }
      const td = document.createElement("td");
      td.className = `${band} ps-metric`;
      td.textContent = `${value.padStart(5)} ${barStr}`;
      tr.appendChild(td);
    };
    cell(r.pid, "ps-l");
    cell(r.user, "ps-l");
    metric(r.cpu, r.cpu === "–" ? "ps-zero" : psBand(r.cpu_val), bars ? model.bars[i].cpu : undefined);
    metric(r.mem, r.mem === "–" ? "ps-zero" : psBand(r.mem_val), bars ? model.bars[i].mem : undefined);
    cell(r.rss, r.rss === "–" ? "ps-zero" : undefined);
    cell(r.start, "ps-l");
    const cmd = cell(r.command, "ps-cmd");
    cmd.title = r.command; // full command on hover (COMMAND is ellipsised)
    psTbody.appendChild(tr);
  });
  psFold.textContent = model.kernel_summary ?? "";
}

async function enhancePs(): Promise<void> {
  // Off => feature disabled; toggle closed if already open.
  if (currentCfg.enhance.ps === "off") return;
  if (!psPanel.hidden) {
    hidePs();
    return;
  }
  const t = activeTab();
  if (!t) return;
  const block = lastPsBlock(t.term);
  if (!block) return; // no ps output to decorate — leave the terminal as-is
  const model = await invoke<PsDecorated | null>("decorate_ps", {
    block,
    cols: t.term.cols,
  });
  if (!model) return; // core declined (too narrow, unparseable) — raw output stands
  const shown = model.rows.length + (model.kernel_count ? model.kernel_count : 0);
  psTitleBase = `Processes — ${model.rows.length} shown${
    model.kernel_count ? `, ${model.kernel_count} kernel folded` : ""
  } (${shown} total)`;
  psModel = model;
  psSort = null; // start in ps's native order
  psSelected = null; // inspector: selection resolves to the first row on render
  psCollapsed.clear();
  repaintPs();
  psPanel.hidden = false;
  refitActive();
  psBody.focus(); // so the panel keys go to it, not the terminal
}
document.getElementById("pspanel-close")!.addEventListener("click", hidePs);

// Panel keys. Inspector (level 1c): ↑↓ select, k signal, y/Y copy, ←→/h l fold. Flat
// (quiet/bars): c/m/p live sort (spec §5). Chords are left to the global dispatcher.
psBody.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.altKey || e.metaKey) return;
  if (isInspector()) {
    switch (e.key) {
      case "ArrowDown":
      case "j":
        movePsSelection(1);
        break;
      case "ArrowUp":
        movePsSelection(-1);
        break;
      case "k":
        signalSelected();
        break;
      case "y":
        copySelected(false);
        break;
      case "Y":
        copySelected(true);
        break;
      case "ArrowLeft":
      case "h":
        foldSelectedGroup(true);
        break;
      case "ArrowRight":
      case "l":
        foldSelectedGroup(false);
        break;
      default:
        return; // let anything else through (e.g. Esc → global handler)
    }
    e.preventDefault();
    return;
  }
  const key =
    e.key === "c" ? "cpu" : e.key === "m" ? "mem" : e.key === "p" ? "pid" : null;
  if (!key) return;
  e.preventDefault();
  psSort = key;
  repaintPs();
});

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
    [parseChord(kb.new_tab), () => void newTab()],
    [parseChord(kb.close_tab), () => active >= 0 && closeTab(active)],
    [parseChord(kb.next_tab), () => switchTab(1)],
    [parseChord(kb.prev_tab), () => switchTab(-1)],
    [parseChord(kb.copy), doCopy],
    [parseChord(kb.search), openSearch],
    [parseChord(kb.palette), () => void openPalette()],
    [parseChord(kb.toggle_man), toggleMan],
    [parseChord(kb.toggle_preview), togglePreview],
    [parseChord(kb.zoom_in), () => zoom(1)],
    [parseChord(kb.zoom_out), () => zoom(-1)],
    [parseChord(kb.zoom_reset), () => zoom(null)],
    [parseChord(kb.help), toggleHelp],
    [parseChord(kb.ai), openAi],
    [parseChord(kb.enhance_ps), () => void enhancePs()],
    [parseChord(kb.explain), () => void explainCurrent()],
  ];
}
rebuildBindings();

document.addEventListener(
  "keydown",
  (e) => {
    // Don't hijack chords while typing in an overlay input (they handle their own keys).
    if (
      document.activeElement === searchInput ||
      document.activeElement === paletteInput ||
      document.activeElement === aiInput
    )
      return;
    // Esc closes the ps panel when it's the frontmost thing (it has no input of its own).
    if (e.key === "Escape" && !psPanel.hidden) {
      e.preventDefault();
      e.stopPropagation();
      hidePs();
      return;
    }
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

// First tab — honors CLI launch options (--hold, --title). New tabs use defaults.
interface LaunchOptions {
  hold: boolean;
  title: string | null;
  exec: boolean;
}
const launch = await invoke<LaunchOptions>("get_launch_options");
await createTab({ hold: launch.hold, title: launch.title ?? undefined });
