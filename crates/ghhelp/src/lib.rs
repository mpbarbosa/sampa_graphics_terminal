//! Parse `gh --help` into a grouped command cheat-sheet for the gh helper panel.
//!
//! `gh` has dozens of subcommands and no useful `man gh`; when the user has typed `gh` and
//! presses the man-panel shortcut, Sampa shows this list instead of a man page. The bridge
//! runs `gh --help` and hands its output here; `parse_gh_help` returns the `name: desc`
//! entries under each `… COMMANDS` section, in order. Choosing/using an entry is the
//! frontend's job — this crate only parses.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no command
//! entries yields `None`, mirroring the other decorator cores.

use serde::{Deserialize, Serialize};

/// One `gh` subcommand: its name, one-line description, and the section it appeared under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhCommand {
    pub name: String,
    pub desc: String,
    /// The section header minus the trailing " COMMANDS" (e.g. "CORE", "ADDITIONAL").
    pub section: String,
}

/// Parse `gh --help` output into its command entries. Section headers are non-indented
/// ALL-CAPS lines ending in `COMMANDS`; entries under them are indented `name: description`
/// lines. Other headers (USAGE, FLAGS, …) end the current section so their lines are
/// ignored. `None` if no entries are found.
pub fn parse_gh_help(output: &str) -> Option<Vec<GhCommand>> {
    let mut section: Option<String> = None;
    let mut out = Vec::new();
    for line in output.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            let h = line.trim();
            section = h
                .strip_suffix(" COMMANDS")
                .filter(|_| h.chars().all(|c| c.is_ascii_uppercase() || c == ' '))
                .map(|s| s.to_string());
            continue;
        }
        // An entry line under a COMMANDS section: `  name: description`.
        if let Some(sec) = &section {
            if let Some((name, desc)) = line.trim().split_once(':') {
                let name = name.trim();
                let desc = desc.trim();
                if !name.is_empty()
                    && !desc.is_empty()
                    && !name.contains(char::is_whitespace)
                {
                    out.push(GhCommand {
                        name: name.to_string(),
                        desc: desc.to_string(),
                        section: sec.clone(),
                    });
                }
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The command's own **description** — the intro paragraph `gh <path> --help` prints before its
/// first section (e.g. for `gh secret`, "Secrets can be set at the repository…"). It's the
/// leading run of non-indented prose lines up to the first blank line; a section header
/// (an ALL-CAPS line like `USAGE`) or an immediately-indented/empty start yields `None`. The
/// original line wrapping is preserved (the man panel renders in a `<pre>`). Lets the cheat-sheet
/// show *what a command does*, not just its subcommand list.
pub fn parse_gh_description(output: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in output.lines() {
        if line.trim().is_empty() {
            if lines.is_empty() {
                continue; // skip any leading blank lines
            }
            break; // end of the first paragraph
        }
        // Stop if the paragraph hasn't started and we're already at a section header (an
        // ALL-CAPS line such as USAGE / CORE COMMANDS), i.e. the help has no description.
        let t = line.trim();
        let is_header = !line.starts_with(char::is_whitespace)
            && t.chars().all(|c| c.is_ascii_uppercase() || c == ' ')
            && t.chars().any(|c| c.is_ascii_uppercase());
        if is_header {
            break;
        }
        lines.push(line.trim_end().to_string());
    }
    let desc = lines.join("\n");
    (!desc.trim().is_empty()).then_some(desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Work seamlessly with GitHub from the command line.

USAGE
  gh <command> <subcommand> [flags]

CORE COMMANDS
  auth:        Authenticate gh and git with GitHub
  pr:          Manage pull requests
  repo:        Manage repositories

ADDITIONAL COMMANDS
  api:         Make an authenticated GitHub API request
  status:      Print information about relevant issues, pull requests, and notifications

FLAGS
  --version   Show gh version

LEARN MORE
  Use `gh <command> <subcommand> --help` for more information about a command.
";

    #[test]
    fn parses_grouped_commands() {
        let cmds = parse_gh_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 5); // 3 core + 2 additional; FLAGS/USAGE/LEARN MORE ignored
        assert_eq!(cmds[0], GhCommand { name: "auth".into(), desc: "Authenticate gh and git with GitHub".into(), section: "CORE".into() });
        assert_eq!(cmds[1].name, "pr");
        assert_eq!(cmds[3].name, "api");
        assert_eq!(cmds[3].section, "ADDITIONAL");
        // The `--version` flag under FLAGS is not a command (section ended).
        assert!(cmds.iter().all(|c| c.name != "--version"));
        // The LEARN MORE prose line (has a colon in the backticked text? no) isn't captured.
        assert!(cmds.iter().all(|c| c.section != "LEARN MORE"));
    }

    #[test]
    fn parses_subcommand_help() {
        // `gh repo --help` uses the same section shape (GENERAL / TARGETED COMMANDS).
        let out = "Work with GitHub repositories.

USAGE
  gh repo <command> [flags]

GENERAL COMMANDS
  create:      Create a new repository
  list:        List repositories

TARGETED COMMANDS
  clone:       Clone a repository locally
  view:        View a repository

INHERITED FLAGS
  --help   Show help for command
";
        let cmds = parse_gh_help(out).unwrap();
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0].section, "GENERAL");
        assert_eq!(cmds[2], GhCommand { name: "clone".into(), desc: "Clone a repository locally".into(), section: "TARGETED".into() });
        assert!(cmds.iter().all(|c| c.name != "--help")); // INHERITED FLAGS ignored
    }

    #[test]
    fn non_gh_help_is_none() {
        assert!(parse_gh_help("").is_none());
        assert!(parse_gh_help("just some text\nwith no commands\n").is_none());
    }

    #[test]
    fn extracts_the_command_description() {
        // `gh secret --help`: a multi-line intro paragraph, then USAGE.
        let out = "Secrets can be set at the repository, or organization level for use in
GitHub Actions or Dependabot. Run `gh help secret set` to learn how to get started.

USAGE
  gh secret <command> [flags]

AVAILABLE COMMANDS
  list:    List secrets
";
        let desc = parse_gh_description(out).unwrap();
        assert!(desc.starts_with("Secrets can be set at the repository"));
        assert!(desc.contains("gh help secret set")); // full paragraph, line breaks preserved
        assert_eq!(desc.lines().count(), 2);
        // Single-line description (top-level `gh --help`).
        assert_eq!(
            parse_gh_description("Work seamlessly with GitHub from the command line.\n\nUSAGE\n"),
            Some("Work seamlessly with GitHub from the command line.".to_string())
        );
        // Help that opens straight at a section header has no description.
        assert!(parse_gh_description("USAGE\n  gh x\n").is_none());
        assert!(parse_gh_description("").is_none());
    }
}
