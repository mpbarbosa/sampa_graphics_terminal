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
// A subdirectory returned by list_dirs (for the cd tree picker).
interface Dir {
  name: string;
  path: string;
}

// A node in the du disk-usage tree (from run_du), sized in KiB.
interface DuNode {
  name: string;
  path: string;
  size_kb: number;
  children: DuNode[];
}

// Memory/swap stats from run_free (KiB), for the free gauge view.
interface MemStats {
  total_kb: number;
  used_kb: number;
  free_kb: number;
  shared_kb: number;
  buff_cache_kb: number;
  available_kb: number;
}
interface SwapStats {
  total_kb: number;
  used_kb: number;
  free_kb: number;
}
interface FreeInfo {
  mem: MemStats;
  swap: SwapStats | null;
}

// Ping latency data from run_ping, for the ping chart view.
interface PingReply {
  seq: number;
  time_ms: number;
}
interface PingReport {
  host: string | null;
  ip: string | null;
  replies: PingReply[];
  transmitted: number;
  received: number;
  loss_pct: number;
  rtt: { min: number; avg: number; max: number; mdev: number } | null;
}

// One filesystem's usage from run_df (KiB), for the df gauge view.
interface FsUsage {
  filesystem: string;
  size_kb: number;
  used_kb: number;
  avail_kb: number;
  use_pct: number;
  mount: string;
}

// Load averages + core count from run_uptime, for the load gauge view.
interface UptimeReport {
  up: string | null;
  users: number | null;
  load1: number;
  load5: number;
  load15: number;
  cores: number;
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
  ["enhance_ps", "Enhance ps / cd / du / free / ping / df / uptime"],
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

// ── cd tree picker (Ctrl+Shift+E while a `cd` command is typed) ───────────────
// Opens a directory tree rooted at the session cwd; the user navigates and chooses a
// directory, which is inserted as the `cd` argument — never executed (insert-never-run).
// Directories load lazily (one level per expand) via the read-only list_dirs command.
const cdEl = document.getElementById("cdtree")!;
const cdRootEl = document.getElementById("cdtree-root")!;
const cdBody = document.getElementById("cdtree-body")!;

interface CdNode {
  name: string;
  path: string;
  depth: number;
  expanded: boolean;
  loaded: boolean;
  children: CdNode[];
}
let cdRoot = ""; // absolute cwd the tree is rooted at
let cdTree: CdNode[] = []; // top-level nodes (subdirs of cdRoot)
let cdVisible: CdNode[] = []; // flattened, respecting expanded state (for ↑↓)
let cdSelected = 0;

function makeCdNodes(dirs: Dir[], depth: number): CdNode[] {
  return dirs.map((d) => ({
    name: d.name,
    path: d.path,
    depth,
    expanded: false,
    loaded: false,
    children: [],
  }));
}

function closeCdTree(): void {
  if (cdEl.hidden) return;
  cdEl.hidden = true;
  cdTree = [];
  cdVisible = [];
  activeTab()?.term.focus();
}

async function openCdTree(): Promise<void> {
  const t = activeTab();
  if (!t) return;
  const cwd = (await invoke<string | null>("get_session_cwd", { session: t.id })) ?? null;
  if (!cwd) return;
  cdRoot = cwd;
  cdTree = makeCdNodes(await invoke<Dir[]>("list_dirs", { path: cwd }), 0);
  cdSelected = 0;
  cdRootEl.textContent = `cd — ${cwd}`;
  renderCdTree();
  cdEl.hidden = false;
  cdBody.focus();
}

// Flatten the tree into the visible node list (depth-first, honoring `expanded`).
function flattenCd(nodes: CdNode[], out: CdNode[]): void {
  for (const n of nodes) {
    out.push(n);
    if (n.expanded) flattenCd(n.children, out);
  }
}

function renderCdTree(): void {
  cdVisible = [];
  flattenCd(cdTree, cdVisible);
  if (cdSelected >= cdVisible.length) cdSelected = Math.max(0, cdVisible.length - 1);
  cdBody.replaceChildren();
  if (cdVisible.length === 0) {
    const empty = document.createElement("div");
    empty.className = "cd-row cd-arrow";
    empty.textContent = "(no subdirectories)";
    cdBody.appendChild(empty);
    return;
  }
  cdVisible.forEach((n, i) => {
    const row = document.createElement("div");
    row.className = i === cdSelected ? "cd-row cd-sel" : "cd-row";
    row.style.paddingLeft = `${14 + n.depth * 16}px`;
    const arrow = document.createElement("span");
    arrow.className = "cd-arrow";
    arrow.textContent = n.expanded ? "▾" : "▸";
    const name = document.createElement("span");
    name.className = "cd-name";
    name.textContent = n.name; // directory name — textContent, never innerHTML
    row.append(arrow, name);
    row.addEventListener("click", () => {
      cdSelected = i;
      void toggleCdNode(n);
    });
    row.addEventListener("dblclick", () => chooseCd(n));
    cdBody.appendChild(row);
  });
  cdBody.querySelector(".cd-sel")?.scrollIntoView({ block: "nearest" });
}

async function expandCdNode(n: CdNode): Promise<void> {
  if (!n.loaded) {
    n.children = makeCdNodes(await invoke<Dir[]>("list_dirs", { path: n.path }), n.depth + 1);
    n.loaded = true;
  }
  n.expanded = true;
  renderCdTree();
}
function collapseCdNode(n: CdNode): void {
  n.expanded = false;
  renderCdTree();
}
async function toggleCdNode(n: CdNode): Promise<void> {
  if (n.expanded) collapseCdNode(n);
  else await expandCdNode(n);
}

function moveCdSelection(delta: number): void {
  if (!cdVisible.length) return;
  cdSelected = Math.max(0, Math.min(cdVisible.length - 1, cdSelected + delta));
  renderCdTree();
}

// Insert the chosen directory as the cd argument, replacing whatever is on the line.
// Never appends a newline — the user's own shell runs it when they press Enter (§13).
function chooseCd(n: CdNode): void {
  const t = activeTab();
  closeCdTree();
  if (!t) return;
  const prefix = cdRoot.endsWith("/") ? cdRoot : `${cdRoot}/`;
  const rel = n.path.startsWith(prefix) ? n.path.slice(prefix.length) : n.path;
  // Quote if the path has shell-significant characters.
  const arg = /[\s'"$`\\()|&;<>*?]/.test(rel) ? `'${rel.replace(/'/g, `'\\''`)}'` : rel;
  // Replace the current line: erase the tracked keystrokes, then write `cd <arg>`.
  const erase = "\x7f".repeat(t.typed.length);
  void invoke("write_session", {
    session: t.id,
    data: bytesToB64(encoder.encode(`${erase}cd ${arg} `)),
  });
  t.term.focus();
}

cdBody.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.altKey || e.metaKey) return;
  const n = cdVisible[cdSelected];
  switch (e.key) {
    case "ArrowDown":
    case "j":
      moveCdSelection(1);
      break;
    case "ArrowUp":
    case "k":
      moveCdSelection(-1);
      break;
    case "ArrowRight":
    case "l":
      if (n && !n.expanded) void expandCdNode(n);
      break;
    case "ArrowLeft":
    case "h":
      if (n && n.expanded) collapseCdNode(n);
      break;
    case "Enter":
      if (n) chooseCd(n);
      break;
    case "Escape":
      closeCdTree();
      break;
    default:
      return;
  }
  e.preventDefault();
});
document.getElementById("cdtree-close")!.addEventListener("click", closeCdTree);
cdEl.addEventListener("mousedown", (e) => {
  if (e.target === cdEl) closeCdTree(); // click backdrop to dismiss
});

// The enhance shortcut (Ctrl+Shift+E) is overloaded: a typed `cd` opens the directory
// tree picker; anything else runs the ps output decorator.
function enhanceShortcut(): void {
  const first = (activeTab()?.typed.trimStart() ?? "").split(/\s+/)[0];
  if (first === "cd") void openCdTree();
  else if (first === "du") void openDuMap();
  else if (first === "free") void openFreeGauge();
  else if (first === "ping") void openPingChart();
  else if (first === "df") void openDfGauge();
  else if (first === "uptime") void openLoadGauge();
  else void enhancePs();
}

// ── uptime load gauge (Ctrl+Shift+E while an `uptime` command is typed) ───────
// Runs a read-only `uptime` (run_uptime) and shows the 1/5/15-minute load averages as bars
// scaled to the CPU core count (100% = load == cores), coloured by that ratio. Because a
// load equal to the core count means "fully busy", the per-core view is what makes a load
// number interpretable. Informational; nothing is composed or run.
const loadEl = document.getElementById("loadgauge")!;
const loadSub = document.getElementById("load-sub")!;
const loadBody = document.getElementById("load-body")!;

function closeLoadGauge(): void {
  if (loadEl.hidden) return;
  loadEl.hidden = true;
  activeTab()?.term.focus();
}

// Colour band for a load/cores ratio — redundant with bar length.
function loadBand(ratio: number): string {
  if (ratio < 0.7) return "#9ece6a";
  if (ratio < 1.0) return "#e0af68";
  if (ratio < 1.5) return "#ff9e64";
  return "#f7768e";
}

function renderLoadGauge(r: UptimeReport): void {
  const cores = Math.max(1, r.cores);
  loadSub.textContent =
    (r.up ? `up ${r.up} · ` : "") +
    (r.users != null ? `${r.users} user${r.users === 1 ? "" : "s"} · ` : "") +
    `${r.cores} cores`;
  loadBody.replaceChildren();
  const rows: [string, number][] = [
    ["1 min", r.load1],
    ["5 min", r.load5],
    ["15 min", r.load15],
  ];
  for (const [label, load] of rows) {
    const ratio = load / cores;
    const row = document.createElement("div");
    row.className = "load-row";
    const lab = document.createElement("span");
    lab.className = "load-label";
    lab.textContent = label;
    const track = document.createElement("div");
    track.className = "load-track";
    const fill = document.createElement("div");
    fill.className = "load-fill";
    fill.style.width = `${Math.min(1, ratio) * 100}%`;
    fill.style.background = loadBand(ratio);
    track.appendChild(fill);
    const val = document.createElement("span");
    val.className = "load-val";
    val.textContent = `${load.toFixed(2)} (${Math.round(ratio * 100)}%)`;
    row.append(lab, track, val);
    loadBody.appendChild(row);
  }
}

async function openLoadGauge(): Promise<void> {
  loadSub.textContent = "";
  loadBody.replaceChildren();
  loadBody.textContent = "Reading load…";
  loadEl.hidden = false;
  loadBody.focus();
  try {
    const report = await invoke<UptimeReport>("run_uptime");
    if (loadEl.hidden) return;
    renderLoadGauge(report);
  } catch (e) {
    if (!loadEl.hidden) loadBody.textContent = String(e);
  }
}

document.getElementById("load-close")!.addEventListener("click", closeLoadGauge);
loadEl.addEventListener("mousedown", (e) => {
  if (e.target === loadEl) closeLoadGauge();
});
loadBody.addEventListener("keydown", (e) => {
  if (e.key === "Escape" || e.key === "Enter") {
    e.preventDefault();
    closeLoadGauge();
  }
});

// ── df disk-free gauge (Ctrl+Shift+E while a `df` command is typed) ───────────
// Runs a read-only `df -k` (run_df) and shows one proportional bar per filesystem — used /
// reserved / free, coloured by use%. Informational; nothing is composed or run.
const dfEl = document.getElementById("dfgauge")!;
const dfBody = document.getElementById("df-body")!;

function closeDfGauge(): void {
  if (dfEl.hidden) return;
  dfEl.hidden = true;
  activeTab()?.term.focus();
}

// Usage colour band by use% — redundant with bar length, never sole signal.
function dfBand(pct: number): string {
  if (pct < 70) return "#9ece6a";
  if (pct < 85) return "#e0af68";
  if (pct < 95) return "#ff9e64";
  return "#f7768e";
}

function renderDfGauge(rows: FsUsage[]): void {
  dfBody.replaceChildren();
  if (rows.length === 0) {
    dfBody.textContent = "No filesystems reported.";
    return;
  }
  // Fullest first — the most actionable.
  const sorted = [...rows].sort((a, b) => b.use_pct - a.use_pct);
  for (const f of sorted) {
    const wrap = document.createElement("div");
    wrap.className = "df-fs";

    const title = document.createElement("div");
    title.className = "df-fs-title";
    const mount = document.createElement("span");
    mount.className = "df-mount";
    mount.textContent = f.mount;
    mount.title = `${f.mount}  (${f.filesystem})`;
    const stat = document.createElement("span");
    stat.className = "df-stat";
    stat.textContent = `${f.use_pct}% · ${fmtKb(f.used_kb)} / ${fmtKb(f.size_kb)} · ${fmtKb(f.avail_kb)} free`;
    title.append(mount, stat);
    wrap.appendChild(title);

    const bar = document.createElement("div");
    bar.className = "df-bar";
    const reserved = Math.max(0, f.size_kb - f.used_kb - f.avail_kb);
    const seg = (grow: number, cls?: string, color?: string) => {
      if (grow <= 0) return;
      const el = document.createElement("div");
      el.className = cls ? `seg ${cls}` : "seg";
      el.style.flexGrow = String(grow);
      el.style.flexBasis = "0";
      if (color) el.style.background = color;
      bar.appendChild(el);
    };
    seg(f.used_kb, undefined, dfBand(f.use_pct)); // used — coloured by band
    seg(reserved, "df-seg-reserved"); // reserved (root-only) blocks
    seg(f.avail_kb, "df-seg-free"); // available
    wrap.appendChild(bar);
    dfBody.appendChild(wrap);
  }
}

async function openDfGauge(): Promise<void> {
  dfBody.replaceChildren();
  dfBody.textContent = "Reading filesystems…";
  dfEl.hidden = false;
  dfBody.focus();
  try {
    const rows = await invoke<FsUsage[]>("run_df");
    if (dfEl.hidden) return;
    renderDfGauge(rows);
  } catch (e) {
    if (!dfEl.hidden) dfBody.textContent = String(e);
  }
}

document.getElementById("df-close")!.addEventListener("click", closeDfGauge);
dfEl.addEventListener("mousedown", (e) => {
  if (e.target === dfEl) closeDfGauge();
});
dfBody.addEventListener("keydown", (e) => {
  if (e.key === "Escape" || e.key === "Enter") {
    e.preventDefault();
    closeDfGauge();
  }
});

// ── ping latency chart (Ctrl+Shift+E while a `ping` command is typed) ─────────
// Runs a bounded read-only `ping` (run_ping) to the host on the typed line and draws the
// per-packet RTTs as a bar chart with a summary — the shape (jitter, spikes, loss) the raw
// stream hides. Informational; nothing is composed or run beyond the ping the user asked
// for. The layout is pixel-dependent so it lives here; the core only parses ping output.
const pingEl = document.getElementById("pingchart")!;
const pingTitle = document.getElementById("ping-title")!;
const pingSub = document.getElementById("ping-sub")!;
const pingBody = document.getElementById("ping-body")!;

function closePingChart(): void {
  if (pingEl.hidden) return;
  pingEl.hidden = true;
  activeTab()?.term.focus();
}

// Latency colour band (ms) — redundant with bar height, never sole signal.
function pingBand(ms: number): string {
  if (ms < 30) return "#9ece6a";
  if (ms < 100) return "#e0af68";
  if (ms < 200) return "#ff9e64";
  return "#f7768e";
}

async function openPingChart(): Promise<void> {
  // Host = the last non-flag token of the typed line (ping's host is normally last).
  const toks = (activeTab()?.typed.trim() ?? "").split(/\s+/).slice(1);
  const host = toks.filter((t) => !t.startsWith("-")).pop() ?? "";
  if (!host) return; // `ping` with no host — nothing to do
  pingTitle.textContent = `ping ${host}`;
  pingSub.textContent = "";
  pingBody.replaceChildren();
  pingBody.textContent = `Pinging ${host}…`;
  pingEl.hidden = false;
  pingBody.focus();
  try {
    const report = await invoke<PingReport>("run_ping", { host });
    if (pingEl.hidden) return; // dismissed while pinging
    renderPingChart(report);
  } catch (e) {
    if (!pingEl.hidden) pingBody.textContent = String(e);
  }
}

function renderPingChart(r: PingReport): void {
  pingTitle.textContent = `ping ${r.host ?? ""}${r.ip ? ` (${r.ip})` : ""}`.trim();
  const rtt = r.rtt;
  pingSub.textContent =
    `${r.transmitted} sent · ${r.received} recv · ${r.loss_pct}% loss` +
    (rtt ? ` · min/avg/max ${rtt.min.toFixed(1)}/${rtt.avg.toFixed(1)}/${rtt.max.toFixed(1)} ms · mdev ${rtt.mdev.toFixed(1)}` : "");

  pingBody.replaceChildren();
  if (r.replies.length === 0) {
    pingBody.textContent =
      r.loss_pct >= 100 ? "All packets lost — no latency to chart." : "No replies to chart.";
    return;
  }
  // Series indexed by sequence 1..maxSeq; a missing seq (loss) is a red floor tick.
  const bySeq = new Map(r.replies.map((p) => [p.seq, p.time_ms]));
  const maxSeq = Math.max(...r.replies.map((p) => p.seq));
  const maxT = rtt?.max ?? Math.max(...r.replies.map((p) => p.time_ms));
  const W = 1000;
  const H = 180;
  const PADX = 4;
  const PADY = 8;
  const chartH = H - PADY * 2;
  const n = maxSeq;
  const bw = (W - PADX * 2) / n;

  const svg = document.createElementNS(SVG_NS, "svg");
  svg.id = "ping-svg";
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("preserveAspectRatio", "none");

  // Average baseline.
  if (rtt && maxT > 0) {
    const y = PADY + chartH - (rtt.avg / maxT) * chartH;
    const avg = document.createElementNS(SVG_NS, "line");
    avg.setAttribute("class", "ping-avg");
    avg.setAttribute("x1", String(PADX));
    avg.setAttribute("x2", String(W - PADX));
    avg.setAttribute("y1", String(y));
    avg.setAttribute("y2", String(y));
    svg.appendChild(avg);
  }

  for (let seq = 1; seq <= n; seq++) {
    const x = PADX + (seq - 1) * bw;
    const t = bySeq.get(seq);
    const g = document.createElementNS(SVG_NS, "g");
    g.setAttribute("class", "ping-cell");
    const rect = document.createElementNS(SVG_NS, "rect");
    rect.setAttribute("x", String(x + bw * 0.1));
    rect.setAttribute("width", String(Math.max(1, bw * 0.8)));
    if (t === undefined) {
      // Lost packet: a short red floor tick.
      const th = Math.min(8, chartH);
      rect.setAttribute("y", String(PADY + chartH - th));
      rect.setAttribute("height", String(th));
      rect.setAttribute("fill", "#f7768e");
      const title = document.createElementNS(SVG_NS, "title");
      title.textContent = `seq ${seq}: lost`;
      g.appendChild(rect);
      g.appendChild(title);
    } else {
      const h = maxT > 0 ? Math.max(1, (t / maxT) * chartH) : 1;
      rect.setAttribute("y", String(PADY + chartH - h));
      rect.setAttribute("height", String(h));
      rect.setAttribute("fill", pingBand(t));
      const title = document.createElementNS(SVG_NS, "title");
      title.textContent = `seq ${seq}: ${t.toFixed(1)} ms`;
      g.appendChild(rect);
      g.appendChild(title);
    }
    svg.appendChild(g);
  }
  pingBody.appendChild(svg);
}

document.getElementById("ping-close")!.addEventListener("click", closePingChart);
pingEl.addEventListener("mousedown", (e) => {
  if (e.target === pingEl) closePingChart();
});
pingBody.addEventListener("keydown", (e) => {
  if (e.key === "Escape" || e.key === "Enter") {
    e.preventDefault();
    closePingChart();
  }
});

// ── free memory gauge (Ctrl+Shift+E while a `free` command is typed) ──────────
// Runs a read-only `free -k` (run_free) and shows RAM + swap as proportional segmented
// gauges. Purely informational — there's no command to compose, so it just displays.
const freeEl = document.getElementById("freegauge")!;
const freeBody = document.getElementById("free-body")!;

function closeFreeGauge(): void {
  if (freeEl.hidden) return;
  freeEl.hidden = true;
  activeTab()?.term.focus();
}

interface Seg {
  label: string;
  kb: number;
  cls: string;
}
// Build one gauge: a title, a proportional segmented bar, and a legend. `extra` items
// appear in the legend only (e.g. "available", which overlaps used/cache and isn't a
// segment). textContent throughout.
function buildGauge(title: string, total: number, segs: Seg[], extra: Seg[]): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "free-gauge";

  const titleRow = document.createElement("div");
  titleRow.className = "free-gauge-title";
  const name = document.createElement("span");
  name.textContent = title;
  const sub = document.createElement("span");
  sub.className = "free-sub";
  const usedish = segs[0];
  const pct = total > 0 ? Math.round((usedish.kb / total) * 100) : 0;
  sub.textContent = `${fmtKb(usedish.kb)} ${usedish.label} of ${fmtKb(total)} (${pct}%)`;
  titleRow.append(name, sub);
  wrap.appendChild(titleRow);

  const bar = document.createElement("div");
  bar.className = "free-bar";
  for (const s of segs) {
    const el = document.createElement("div");
    el.className = `seg ${s.cls}`;
    el.style.flexGrow = String(Math.max(0, s.kb));
    el.style.flexBasis = "0";
    el.title = `${s.label}: ${fmtKb(s.kb)}`;
    bar.appendChild(el);
  }
  wrap.appendChild(bar);

  const legend = document.createElement("div");
  legend.className = "free-legend";
  for (const s of [...segs, ...extra]) {
    const item = document.createElement("span");
    item.className = "item";
    const sw = document.createElement("span");
    sw.className = `swatch ${s.cls}`;
    const txt = document.createElement("span");
    const p = total > 0 ? Math.round((s.kb / total) * 100) : 0;
    txt.textContent = `${s.label} ${fmtKb(s.kb)} (${p}%)`;
    item.append(sw, txt);
    legend.appendChild(item);
  }
  wrap.appendChild(legend);
  return wrap;
}

function renderFreeGauge(info: FreeInfo): void {
  freeBody.replaceChildren();
  const m = info.mem;
  freeBody.appendChild(
    buildGauge(
      "RAM",
      m.total_kb,
      [
        { label: "used", kb: m.used_kb, cls: "seg-used" },
        { label: "buff/cache", kb: m.buff_cache_kb, cls: "seg-cache" },
        { label: "free", kb: m.free_kb, cls: "seg-free" },
      ],
      // "available" overlaps used/cache (free + reclaimable), so it's legend-only.
      [{ label: "available", kb: m.available_kb, cls: "seg-avail" }],
    ),
  );
  if (info.swap && info.swap.total_kb > 0) {
    const s = info.swap;
    freeBody.appendChild(
      buildGauge(
        "Swap",
        s.total_kb,
        [
          { label: "used", kb: s.used_kb, cls: "sw-used" },
          { label: "free", kb: s.free_kb, cls: "sw-free" },
        ],
        [],
      ),
    );
  }
}

async function openFreeGauge(): Promise<void> {
  freeBody.replaceChildren();
  freeBody.textContent = "Reading memory…";
  freeEl.hidden = false;
  freeBody.focus();
  try {
    const info = await invoke<FreeInfo>("run_free");
    if (freeEl.hidden) return; // dismissed
    renderFreeGauge(info);
  } catch (e) {
    if (!freeEl.hidden) freeBody.textContent = String(e);
  }
}

document.getElementById("free-close")!.addEventListener("click", closeFreeGauge);
freeEl.addEventListener("mousedown", (e) => {
  if (e.target === freeEl) closeFreeGauge(); // click backdrop to dismiss
});
freeBody.addEventListener("keydown", (e) => {
  if (e.key === "Escape" || e.key === "Enter") {
    e.preventDefault();
    closeFreeGauge();
  }
});

// ── du disk-usage treemap (Ctrl+Shift+E while a `du` command is typed) ────────
// Runs a read-only, timeout-bounded `du` on the cwd (run_du) and renders its tree as a
// squarified treemap: bigger box = more disk. Click a box to zoom into that directory,
// Backspace to go up, Enter to `cd` to the directory you've navigated to (inserted, never
// run). The squarify layout is a rendering concern (pixel-dependent), so it lives here;
// the core only parses du into the sized tree.
const duEl = document.getElementById("dumap")!;
const duCrumb = document.getElementById("dumap-crumb")!;
const duBody = document.getElementById("dumap-body")!;
const SVG_NS = "http://www.w3.org/2000/svg";
// A muted, theme-consistent palette cycled across top-level boxes.
const DU_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e", "#a9b1d6"];
let duStack: DuNode[] = []; // view stack; last element is the current (zoomed) directory

function closeDuMap(): void {
  if (duEl.hidden) return;
  duEl.hidden = true;
  duStack = [];
  activeTab()?.term.focus();
}

async function openDuMap(): Promise<void> {
  const t = activeTab();
  if (!t) return;
  const cwd = (await invoke<string | null>("get_session_cwd", { session: t.id })) ?? null;
  if (!cwd) return;
  duCrumb.textContent = "Scanning disk usage…";
  duBody.replaceChildren();
  duBody.textContent = "Running du…";
  duEl.hidden = false;
  duBody.focus();
  try {
    const root = await invoke<DuNode>("run_du", { path: cwd });
    if (duEl.hidden) return; // dismissed while scanning
    duStack = [root];
    renderDuMap();
  } catch (e) {
    if (!duEl.hidden) duBody.textContent = String(e);
  }
}

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
// Squarified treemap (Bruls et al.): lay `areas` (already sorted desc) into `rect`,
// keeping each cell's aspect ratio near 1. Returns one rect per area, in the same order.
function squarify(areas: number[], rect: Rect): Rect[] {
  const rects: Rect[] = [];
  let free = { ...rect };
  let i = 0;
  const worst = (row: number[], side: number): number => {
    const sum = row.reduce((a, b) => a + b, 0);
    const max = Math.max(...row);
    const min = Math.min(...row);
    const s2 = sum * sum;
    const side2 = side * side;
    return Math.max((side2 * max) / s2, s2 / (side2 * min));
  };
  while (i < areas.length) {
    const vertical = free.w > free.h; // wide → stack a column at the left
    const side = vertical ? free.h : free.w;
    const row = [areas[i]];
    i++;
    while (i < areas.length && worst([...row, areas[i]], side) <= worst(row, side)) {
      row.push(areas[i]);
      i++;
    }
    const rowSum = row.reduce((a, b) => a + b, 0);
    if (vertical) {
      const colW = free.h > 0 ? rowSum / free.h : 0;
      let yy = free.y;
      for (const a of row) {
        const rh = colW > 0 ? a / colW : 0;
        rects.push({ x: free.x, y: yy, w: colW, h: rh });
        yy += rh;
      }
      free = { x: free.x + colW, y: free.y, w: free.w - colW, h: free.h };
    } else {
      const rowH = free.w > 0 ? rowSum / free.w : 0;
      let xx = free.x;
      for (const a of row) {
        const rw = rowH > 0 ? a / rowH : 0;
        rects.push({ x: xx, y: free.y, w: rw, h: rowH });
        xx += rw;
      }
      free = { x: free.x, y: free.y + rowH, w: free.w, h: free.h - rowH };
    }
  }
  return rects;
}

function renderDuCrumb(): void {
  duCrumb.replaceChildren();
  duStack.forEach((node, i) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "du-sep";
      sep.textContent = " / ";
      duCrumb.appendChild(sep);
    }
    const seg = document.createElement("span");
    seg.className = "du-seg";
    seg.textContent = `${node.name} (${fmtKb(node.size_kb)})`;
    seg.addEventListener("click", () => {
      duStack = duStack.slice(0, i + 1); // jump back to this level
      renderDuMap();
    });
    duCrumb.appendChild(seg);
  });
}

function renderDuMap(): void {
  renderDuCrumb();
  const node = duStack[duStack.length - 1];
  duBody.replaceChildren();
  const W = duBody.clientWidth - 12; // minus padding
  const H = duBody.clientHeight - 12;
  if (W <= 0 || H <= 0) return;

  // Cells: child directories, plus a non-navigable remainder for this dir's own files.
  type Cell = { node: DuNode | null; size: number; label: string };
  const cells: Cell[] = node.children
    .filter((c) => c.size_kb > 0)
    .map((c) => ({ node: c, size: c.size_kb, label: c.name }));
  const childSum = node.children.reduce((a, c) => a + c.size_kb, 0);
  const own = node.size_kb - childSum;
  if (own > 0 && cells.length > 0) cells.push({ node: null, size: own, label: "(files here)" });
  if (cells.length === 0) {
    duBody.textContent = `${node.name} — ${fmtKb(node.size_kb)}, no subdirectories`;
    return;
  }

  // squarify expects areas summing to the rect's pixel area — scale the raw KiB sizes so
  // total area == W*H (otherwise the first box overflows and the rest land off-screen).
  const totalSize = cells.reduce((a, c) => a + c.size, 0);
  const scale = totalSize > 0 ? (W * H) / totalSize : 0;
  const rects = squarify(
    cells.map((c) => c.size * scale),
    { x: 6, y: 6, w: W, h: H },
  );
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.id = "dumap-svg";
  svg.setAttribute("viewBox", `0 0 ${W + 12} ${H + 12}`);
  cells.forEach((cell, i) => {
    const r = rects[i];
    if (r.w < 1 || r.h < 1) return;
    const g = document.createElementNS(SVG_NS, "g");
    const navigable = !!cell.node && cell.node.children.length > 0;
    g.setAttribute("class", navigable ? "du-cell" : "du-cell du-leaf");
    const rect = document.createElementNS(SVG_NS, "rect");
    rect.setAttribute("x", String(r.x));
    rect.setAttribute("y", String(r.y));
    rect.setAttribute("width", String(r.w));
    rect.setAttribute("height", String(r.h));
    rect.setAttribute("fill", cell.node ? DU_COLORS[i % DU_COLORS.length] : "#414868");
    g.appendChild(rect);
    // Label only when the box is big enough to read.
    if (r.w > 46 && r.h > 18) {
      const label = document.createElementNS(SVG_NS, "text");
      label.setAttribute("x", String(r.x + 4));
      label.setAttribute("y", String(r.y + 14));
      label.textContent = `${cell.label} ${fmtKb(cell.size)}`;
      g.appendChild(label);
    }
    const title = document.createElementNS(SVG_NS, "title");
    title.textContent = `${cell.label} — ${fmtKb(cell.size)}`; // hover tooltip
    g.appendChild(title);
    if (navigable) {
      g.addEventListener("click", () => {
        duStack.push(cell.node!);
        renderDuMap();
      });
    }
    svg.appendChild(g);
  });
  duBody.appendChild(svg);
}

// cd to the directory currently in view (Enter). Inserted at the prompt, never run.
function cdToDuView(): void {
  const node = duStack[duStack.length - 1];
  const t = activeTab();
  if (!node || !t) return;
  closeDuMap();
  const arg = /[\s'"$`\\()|&;<>*?]/.test(node.path)
    ? `'${node.path.replace(/'/g, `'\\''`)}'`
    : node.path;
  const erase = "\x7f".repeat(t.typed.length);
  void invoke("write_session", {
    session: t.id,
    data: bytesToB64(encoder.encode(`${erase}cd ${arg} `)),
  });
  t.term.focus();
}

duBody.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.altKey || e.metaKey) return;
  if (e.key === "Backspace") {
    if (duStack.length > 1) {
      duStack.pop();
      renderDuMap();
    }
  } else if (e.key === "Enter") {
    cdToDuView();
  } else if (e.key === "Escape") {
    closeDuMap();
  } else {
    return;
  }
  e.preventDefault();
});
document.getElementById("dumap-close")!.addEventListener("click", closeDuMap);
duEl.addEventListener("mousedown", (e) => {
  if (e.target === duEl) closeDuMap(); // click backdrop to dismiss
});

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
    [parseChord(kb.enhance_ps), enhanceShortcut],
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
