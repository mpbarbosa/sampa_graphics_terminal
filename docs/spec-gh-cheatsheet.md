# Spec — `gh` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/ghhelp` (`sampa-ghhelp`),
  the `gh_help` bridge command, and `showGhHelp` in `src/main.ts`, rendered in the man panel).
- **Applies to:** showing a grouped list of GitHub CLI (`gh`) commands in the man panel when
  the typed command is `gh`. Language-agnostic behavioral contract so any frontend behaves
  identically.

## 1. Purpose

`gh` has dozens of subcommands and no useful `man gh`; discovering what it can do means
reading `gh --help`. Sampa surfaces that in the **man panel**: when the command on the line
is `gh`, the panel shows a grouped, aligned cheat-sheet of gh's commands instead of a man
page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`**
  — **not** the enhance shortcut (`Ctrl+Shift+E`). The man panel detects the command from the
  tracked keystrokes (`tab.typed` / OSC 133), gated to real `$PATH` commands (`gh` qualifies).
- When the detected command is **`gh`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. This works on both the man
  panel's keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand — for *every* subcommand.** The frontend reads the **subcommand
  path** from the typed line — the leading subcommand-like tokens after `gh` (stopping at the
  first flag) — and runs `gh <path…> --help`, so it drills to any depth: `gh` shows the
  top-level commands, `gh repo` shows repo's commands, `gh pr` shows pr's, and so on. Nothing
  is `repo`-specific — the path is generic.
- **Leaf fallback.** A path with no `… COMMANDS` sections is a **leaf** action (`gh repo view`,
  `gh pr checkout`, `gh auth login`, `gh release create`, …). Rather than blank the panel, the
  frontend then shows that command's **own `--help`** (usage + flags) as plain text. So every
  subcommand — container or leaf, at any depth — surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `gh --help` (read-only, **local — no network**, instant). No shell.
- The core parses the output into `GhCommand { name, desc, section }` entries: under each
  non-indented ALL-CAPS `… COMMANDS` header (CORE, GITHUB ACTIONS, ALIAS, ADDITIONAL, …),
  the indented `name: description` lines. Non-command sections (USAGE, FLAGS, LEARN MORE)
  end the current section and are ignored. **Fails safe:** no entries → nothing (the man
  panel hides / falls through).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned cheat-sheet: **the command's own
  description first** (the intro paragraph `gh <path> --help` prints — e.g. "Secrets can be set
  at the repository…" for `gh secret` — line wrapping preserved), then each `… COMMANDS` section
  header, then `  <name padded>  <description>` rows so names line up. Title: `gh — commands`.
  Text reaches the DOM via `textContent` only.

## 5. Architecture mapping

- **`crates/ghhelp` (`sampa-ghhelp`)** — headless parse core. `parse_gh_help(output) ->
  Vec<GhCommand>` (grouped subcommands) and `parse_gh_description(output) -> Option<String>`
  (the command's intro paragraph), both fail-safe-to-`None`, mirroring the other decorator
  cores. Pure `std` + serde — **no shell, no Tauri**. Tested against sample and real `gh --help`.
- **Bridge** — `gh_help(args)` runs `gh <path…> --help` and returns the parsed entries;
  `gh_help_raw(args)` returns the same command's raw help text (C0-stripped) for the leaf
  fallback. Both share a `run_gh_help` helper (flag-shaped args dropped, no shell, no network).
- **Frontend** — `showGhHelp` formats the grouped/aligned command list; on a leaf (no list) it
  falls back to `showGhHelpRaw`, which shows the raw `gh … — help` text. `showMan` routes `gh`
  to it. Both render via `textContent` only.

## 6. Relationship to existing docs

Sits alongside the man panel (`spec` covered in DESIGN.md §10.2) and the `Ctrl+Shift+E`
decorator family (`spec-ps-output-enhancement.md`, `-cd-tree-picker`, `-du-treemap`,
`-free-gauge`, `-ping-chart`, `-df-gauge`, `-load-gauge`) — but it is the first decorator to
overload the **man** shortcut rather than the enhance shortcut. The same man-panel override
could later cover other subcommand-rich CLIs with weak man pages (docker, cargo, kubectl) via
their `--help` output.
