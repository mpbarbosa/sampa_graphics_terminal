//! Configuration model for the Sampa terminal (DESIGN.md §11).
//!
//! A plain, serde-backed data model parsed from TOML. Every field has a default,
//! so a partial (or missing) config file still yields a complete [`Config`] — the
//! file only needs to mention what the user wants to change. The crate is headless
//! (no GUI dependency) so it stays unit-testable; the frontend consumes the parsed
//! struct over IPC and maps it onto xterm.js.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level configuration. `#[serde(default)]` on every struct means any omitted
/// table or key falls back to its default rather than failing to parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: Font,
    pub colors: Colors,
    pub window: Window,
    pub scrollback: Scrollback,
    pub shell: Shell,
    pub cursor: Cursor,
    pub bell: Bell,
    pub keybindings: Keybindings,
    pub rendering: Rendering,
    pub clipboard: Clipboard,
    /// Signature-feature toggles (wired up in M4); present now so configs are
    /// forward-compatible.
    pub features: Features,
    /// Opt-in Claude-API command suggester. Off by default — it is Sampa's only
    /// network surface (§13).
    pub ai: Ai,
    /// `ps(1)` output enhancement (docs/spec-ps-output-enhancement.md). Presentation
    /// only — piped/redirected output is always byte-identical.
    pub enhance: Enhance,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: Font::default(),
            colors: Colors::default(),
            window: Window::default(),
            scrollback: Scrollback::default(),
            shell: Shell::default(),
            cursor: Cursor::default(),
            bell: Bell::default(),
            keybindings: Keybindings::default(),
            rendering: Rendering::default(),
            clipboard: Clipboard::default(),
            features: Features::default(),
            ai: Ai::default(),
            enhance: Enhance::default(),
        }
    }
}

/// How to handle an app's OSC 52 request to *write* the system clipboard (§13). Reads
/// are always denied (never answered) so terminal content can't exfiltrate the
/// clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Osc52Policy {
    /// Prompt for confirmation, showing what would be copied (default).
    Ask,
    /// Copy without prompting.
    Allow,
    /// Ignore the request.
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clipboard {
    pub osc52_write: Osc52Policy,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { osc52_write: Osc52Policy::Ask }
    }
}

/// Rendering options (M5). `gpu` selects the WebGL renderer (falls back to canvas if
/// the GL context is lost); `images` enables inline sixel/iTerm image display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rendering {
    pub gpu: bool,
    pub images: bool,
}

impl Default for Rendering {
    fn default() -> Self {
        Self { gpu: true, images: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// CSS-style fallback list; the first family with the needed glyphs wins.
    pub family: String,
    pub size: f32,
    /// Programming ligatures. Off is safest in a terminal; a toggle regardless.
    pub ligatures: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "\"MesloLGS NF\", \"Hack Nerd Font\", ui-monospace, \"JetBrains Mono\", Menlo, Consolas, monospace".into(),
            size: 14.0,
            ligatures: false,
        }
    }
}

/// Theme colors. Hex strings (`#rrggbb`); the frontend validates/parses them.
/// Defaults are the Tokyo Night palette used since M0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: "#16161e".into(),
            foreground: "#c0caf5".into(),
            cursor: "#c0caf5".into(),
            selection: "#283457".into(),
            black: "#15161e".into(),
            red: "#f7768e".into(),
            green: "#9ece6a".into(),
            yellow: "#e0af68".into(),
            blue: "#7aa2f7".into(),
            magenta: "#bb9af7".into(),
            cyan: "#7dcfff".into(),
            white: "#a9b1d6".into(),
            bright_black: "#414868".into(),
            bright_red: "#f7768e".into(),
            bright_green: "#9ece6a".into(),
            bright_yellow: "#e0af68".into(),
            bright_blue: "#7aa2f7".into(),
            bright_magenta: "#bb9af7".into(),
            bright_cyan: "#7dcfff".into(),
            bright_white: "#c0caf5".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    /// Inner padding in CSS pixels.
    pub padding_x: u16,
    pub padding_y: u16,
    /// Startup grid size (the frontend still fits to the real window).
    pub cols: u16,
    pub rows: u16,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            padding_x: 8,
            padding_y: 8,
            cols: 80,
            rows: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scrollback {
    pub lines: u32,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self { lines: 10_000 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    /// Shell binary. `None` (unset) inherits `$SHELL`.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Start as a login shell (prepends `-l`-style arg handling to the caller).
    pub login: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            program: None,
            args: Vec::new(),
            login: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cursor {
    pub style: CursorStyle,
    pub blink: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            style: CursorStyle::Block,
            blink: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bell {
    pub visual: bool,
    pub audible: bool,
}

impl Default for Bell {
    fn default() -> Self {
        Self {
            visual: true,
            audible: false,
        }
    }
}

/// Chord → action bindings, reserved to the `Ctrl+Shift+*` namespace so shell keys
/// aren't shadowed (DESIGN.md §8.4). Values are chords like `"Ctrl+Shift+T"`; the
/// frontend parses and matches them (key tokens are physical, layout-independent —
/// e.g. `T`, `Right`, `Tab`, `Equal`, `Minus`, `0`). Unbound chords fall through to
/// the shell. Paste stays on the native Ctrl+Shift+V / middle-click path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybindings {
    pub new_tab: String,
    pub close_tab: String,
    pub next_tab: String,
    pub prev_tab: String,
    pub copy: String,
    pub search: String,
    pub palette: String,
    pub toggle_man: String,
    pub toggle_preview: String,
    pub zoom_in: String,
    pub zoom_out: String,
    pub zoom_reset: String,
    pub help: String,
    pub ai: String,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            new_tab: "Ctrl+Shift+T".into(),
            close_tab: "Ctrl+Shift+W".into(),
            next_tab: "Ctrl+Shift+Right".into(),
            prev_tab: "Ctrl+Shift+Left".into(),
            copy: "Ctrl+Shift+C".into(),
            search: "Ctrl+Shift+F".into(),
            palette: "Ctrl+Shift+P".into(),
            toggle_man: "Ctrl+Shift+M".into(),
            toggle_preview: "Ctrl+Shift+R".into(),
            zoom_in: "Ctrl+Shift+Equal".into(),
            zoom_out: "Ctrl+Shift+Minus".into(),
            zoom_reset: "Ctrl+Shift+0".into(),
            help: "Ctrl+Shift+Slash".into(), // Ctrl+Shift+? on a US layout
            ai: "Ctrl+Shift+A".into(),       // ask the Claude-API command suggester
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
    pub palette: bool,
    pub man: bool,
    pub preview: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            palette: false,
            man: false,
            preview: false,
        }
    }
}

/// Opt-in Claude-API command suggester (§13). Disabled by default because it is
/// Sampa's only outbound network surface and sends the user's request to a third
/// party. The API key is read from the `ANTHROPIC_API_KEY` environment variable at
/// runtime — never from this file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ai {
    /// Master switch. When false, the suggester is inert and no request is ever made.
    pub enabled: bool,
    /// Model id (default `claude-opus-5`; use `claude-haiku-4-5` for lower latency/cost).
    pub model: String,
    /// Messages endpoint. Point at a local/OpenAI-compatible-proxy URL to keep data
    /// on-device instead of sending it to the hosted Claude API.
    pub endpoint: String,
    /// When true, attach recent terminal output/cwd to the request. Off by default —
    /// terminal output can contain secrets, and this is the data that leaves the machine.
    pub send_context: bool,
}

impl Default for Ai {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "claude-opus-5".into(),
            endpoint: "https://api.anthropic.com/v1/messages".into(),
            send_context: false,
        }
    }
}

/// The `ps(1)` output-enhancement level (spec §3). Levels are progressive — each builds
/// on the one below and needs more terminal width; below a level's width threshold the
/// emulator falls back one level. Piped/redirected `ps` is never enhanced regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PsEnhance {
    /// No enhancement anywhere — raw passthrough (the escape hatch).
    Off,
    /// Level 1a — text + colour only: zero elision, size units, kernel fold. SSH-safe.
    Quiet,
    /// Level 1b — 1a plus signal bars, live sort, and a position readout.
    Bars,
    /// Level 1c — the two-pane, selectable inspector.
    Inspector,
}

/// `ps(1)` output enhancement (spec §3). Default level is `quiet` (the SSH-safe 1a); the
/// width thresholds gate the richer levels and drive one-level fallback on narrow
/// terminals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Enhance {
    /// The `ps` enhancement level: `off | quiet | bars | inspector`.
    pub ps: PsEnhance,
    /// Below this width, no enhancement is applied at all (raw passthrough). Default 80.
    pub min_width: u16,
    /// Below this width, the `bars` level falls back to `quiet`. Default 100.
    pub min_width_bars: u16,
    /// Below this width, the `inspector` level falls back to `bars`. Default 120.
    pub min_width_inspector: u16,
}

impl Default for Enhance {
    fn default() -> Self {
        Self {
            ps: PsEnhance::Quiet,
            min_width: 80,
            min_width_bars: 100,
            min_width_inspector: 120,
        }
    }
}

impl Config {
    /// Parse a config from a TOML string. Unknown keys are rejected so typos
    /// surface instead of silently doing nothing.
    pub fn from_toml(s: &str) -> Result<Config> {
        toml::from_str(s).context("parsing config TOML").and_then(|c: Config| {
            c.validate()?;
            Ok(c)
        })
    }

    /// Load from `path`; a missing file yields [`Config::default`] (not an error —
    /// running without a config is normal). A malformed file *is* an error.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Config::from_toml(&s)
                .with_context(|| format!("in config file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Light semantic validation beyond types.
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.font.size > 0.0, "font.size must be positive");
        anyhow::ensure!(self.window.cols > 0 && self.window.rows > 0, "window cols/rows must be positive");
        Ok(())
    }
}

/// Resolve the config file path: `$XDG_CONFIG_HOME/sampa/config.toml`, falling back
/// to `$HOME/.config/sampa/config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("sampa").join("config.toml"))
}

/// A documented default config, suitable for writing to disk on first run so the
/// user has something to edit. Every value here equals [`Config::default`].
pub const DEFAULT_CONFIG_TOML: &str = r##"# Sampa terminal configuration.
# Every setting is optional; delete a line to fall back to its default.
# Edits are applied live — no restart needed.

[font]
family = "\"MesloLGS NF\", \"Hack Nerd Font\", ui-monospace, \"JetBrains Mono\", Menlo, Consolas, monospace"
size = 14.0
ligatures = false

[colors]                 # hex "#rrggbb"; defaults are Tokyo Night
background = "#16161e"
foreground = "#c0caf5"
cursor     = "#c0caf5"
selection  = "#283457"
black = "#15161e"
red = "#f7768e"
green = "#9ece6a"
yellow = "#e0af68"
blue = "#7aa2f7"
magenta = "#bb9af7"
cyan = "#7dcfff"
white = "#a9b1d6"
bright_black = "#414868"
bright_red = "#f7768e"
bright_green = "#9ece6a"
bright_yellow = "#e0af68"
bright_blue = "#7aa2f7"
bright_magenta = "#bb9af7"
bright_cyan = "#7dcfff"
bright_white = "#c0caf5"

[window]
padding_x = 8
padding_y = 8
cols = 80
rows = 24

[scrollback]
lines = 10000

[shell]
# program = "/usr/bin/zsh"   # unset => $SHELL
args = []
login = false

[cursor]
style = "block"   # block | bar | underline
blink = true

[bell]
visual = true
audible = false

[keybindings]        # app actions; reserved to the Ctrl+Shift+* namespace
new_tab = "Ctrl+Shift+T"
close_tab = "Ctrl+Shift+W"
next_tab = "Ctrl+Shift+Right"
prev_tab = "Ctrl+Shift+Left"
copy = "Ctrl+Shift+C"
search = "Ctrl+Shift+F"
palette = "Ctrl+Shift+P"
toggle_man = "Ctrl+Shift+M"
toggle_preview = "Ctrl+Shift+R"
zoom_in = "Ctrl+Shift+Equal"
zoom_out = "Ctrl+Shift+Minus"
zoom_reset = "Ctrl+Shift+0"
help = "Ctrl+Shift+Slash"  # Ctrl+Shift+? — shows the shortcuts overlay
ai = "Ctrl+Shift+A"        # ask the Claude-API command suggester (opt-in; see [ai])

[rendering]
gpu = true          # WebGL renderer (falls back to canvas if unavailable)
images = true       # inline sixel / iTerm image display

[clipboard]
osc52_write = "ask" # app clipboard writes (OSC 52): ask | allow | deny. Reads are always denied.

[features]          # signature features, wired up in M4
palette = false
man = false
preview = false

[ai]                # opt-in Claude-API command suggester — Sampa's ONLY network surface (§13)
enabled = false     # master switch; off means no request is ever made
model = "claude-opus-5"                       # or claude-haiku-4-5 for lower latency/cost
endpoint = "https://api.anthropic.com/v1/messages"  # point at a local proxy to keep data on-device
send_context = false  # attach recent output/cwd (may contain secrets) to the request
# The API key comes from the ANTHROPIC_API_KEY environment variable, never this file.

[enhance]               # ps(1) output enhancement — presentation only; pipes stay byte-identical
ps = "quiet"            # off | quiet | bars | inspector (progressive; quiet is SSH-safe 1a)
min_width = 80          # below this many columns, no enhancement at all (raw passthrough)
min_width_bars = 100    # below this, the bars level falls back to quiet
min_width_inspector = 120  # below this, the inspector level falls back to bars
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybindings_default_and_override() {
        assert_eq!(Config::default().keybindings.new_tab, "Ctrl+Shift+T");
        let c = Config::from_toml("[keybindings]\nnext_tab = \"Ctrl+Shift+Tab\"\n").unwrap();
        assert_eq!(c.keybindings.next_tab, "Ctrl+Shift+Tab");
        // Unspecified bindings keep their defaults.
        assert_eq!(c.keybindings.close_tab, "Ctrl+Shift+W");
    }

    #[test]
    fn default_is_stable() {
        let c = Config::default();
        assert_eq!(c.font.size, 14.0);
        assert_eq!(c.colors.background, "#16161e");
        assert_eq!(c.cursor.style, CursorStyle::Block);
        assert_eq!(c.scrollback.lines, 10_000);
        assert!(c.shell.program.is_none());
    }

    #[test]
    fn enhance_default_and_override() {
        // Default is the SSH-safe 1a level with the spec's width thresholds.
        let d = Config::default().enhance;
        assert_eq!(d.ps, PsEnhance::Quiet);
        assert_eq!(d.min_width, 80);
        assert_eq!(d.min_width_bars, 100);
        assert_eq!(d.min_width_inspector, 120);

        // Level parses from its lowercase name; unspecified fields keep defaults.
        let c = Config::from_toml("[enhance]\nps = \"inspector\"\n").unwrap();
        assert_eq!(c.enhance.ps, PsEnhance::Inspector);
        assert_eq!(c.enhance.min_width_bars, 100);

        // The escape hatch.
        let off = Config::from_toml("[enhance]\nps = \"off\"\n").unwrap();
        assert_eq!(off.enhance.ps, PsEnhance::Off);

        // A bad level name is rejected, not silently ignored.
        assert!(Config::from_toml("[enhance]\nps = \"fancy\"\n").is_err());
    }

    #[test]
    fn shipped_default_toml_matches_default() {
        // The documented default we write to disk must parse back to Config::default.
        let parsed = Config::from_toml(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn partial_config_fills_the_rest() {
        let c = Config::from_toml(
            r#"
            [font]
            size = 18.0

            [cursor]
            style = "bar"
        "#,
        )
        .unwrap();
        // Overridden:
        assert_eq!(c.font.size, 18.0);
        assert_eq!(c.cursor.style, CursorStyle::Bar);
        // Defaulted (not mentioned):
        assert_eq!(c.colors.background, "#16161e");
        assert_eq!(c.scrollback.lines, 10_000);
        assert!(c.font.family.contains("MesloLGS NF"));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = Config::from_toml("[font]\nsiez = 18.0\n").unwrap_err();
        assert!(err.to_string().to_lowercase().contains("parsing config"));
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::from_toml("[font]\nsize = 0.0\n").is_err());
        assert!(Config::from_toml("[window]\ncols = 0\n").is_err());
    }

    #[test]
    fn missing_file_is_default_not_error() {
        let c = Config::load(Path::new("/no/such/sampa/config.toml")).unwrap();
        assert_eq!(c, Config::default());
    }
}
