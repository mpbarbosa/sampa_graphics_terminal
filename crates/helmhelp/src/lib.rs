//! Parse `helm --help` into a command cheat-sheet for the helm helper panel.
//!
//! Like `kubectl`, `helm` has many subcommands; when the user has typed `helm` and presses the
//! man-panel shortcut, Sampa shows this list instead of a man page. The bridge runs
//! `helm --help` and hands its output here; `parse_helm_help` returns the `name → desc` entries
//! under each command section, in order.
//!
//! helm is a **pure-cobra** CLI, so its help — at the top level and for every subcommand
//! (`helm repo`, `helm get`, …) — lists commands under a single `Available Commands:` header
//! with `  <name>  <description>` rows. That's the same shape a kubectl subcommand uses; the
//! header rule here is the same "a non-indented line ending in `:` that mentions command"
//! (also tolerating a qualified header, should helm ever add one). Non-command sections (Flags,
//! Usage, Examples) end the current section so their lines are ignored.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no command
//! entries yields `None` (a leaf like `helm install --help`), mirroring the other help cores.

use serde::{Deserialize, Serialize};

/// One `helm` subcommand: its name, one-line description, and the section it appeared under
/// (almost always `"Available Commands"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelmCommand {
    pub name: String,
    pub desc: String,
    pub section: String,
}

/// True for a helm command-section header: a non-indented line ending in `:` whose text mentions
/// a command group (cobra's `Available Commands:`, or any `… Commands:`). Excludes `Flags:` /
/// `Usage:` / `Examples:`.
fn is_command_header(header: &str) -> bool {
    header.ends_with(':') && header.to_ascii_lowercase().contains("command")
}

/// Parse `helm --help` output into its command entries. A section header is a non-indented line
/// ending in `:` that mentions "command"; the stored section is that line minus the trailing
/// colon. Rows under it are indented `name  description` lines (split on the first run of 2+
/// spaces). Any other non-indented line (Flags, Usage, the tagline) ends the current section.
/// `None` if no entries are found.
pub fn parse_helm_help(output: &str) -> Option<Vec<HelmCommand>> {
    let mut section: Option<String> = None;
    let mut out = Vec::new();
    for line in output.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            let h = line.trim();
            section = is_command_header(h).then(|| h.trim_end_matches(':').to_string());
            continue;
        }
        let Some(sec) = &section else { continue };
        if line.trim().is_empty() {
            continue; // a blank line does not end the section; the next header does
        }
        if let Some((name, desc)) = split_columns(line.trim()) {
            let name = name.trim();
            let desc = desc.trim();
            if !name.is_empty()
                && !desc.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.push(HelmCommand {
                    name: name.to_string(),
                    desc: desc.to_string(),
                    section: sec.clone(),
                });
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Split a row on its first run of two-or-more spaces into `(left, right)`.
fn split_columns(row: &str) -> Option<(&str, &str)> {
    let bytes = row.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b' ' {
            return Some((&row[..i], row[i..].trim_start()));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "The Kubernetes package manager

Common actions for Helm:
...

Usage:
  helm [command]

Available Commands:
  create      create a new chart with the given name
  dependency  manage a chart's dependencies
  get         download extended information of a named release
  install     install a chart
  repo        add, list, remove, update, and index chart repositories

Flags:
      --burst-limit int   client-side default throttling limit (default 100)
  -h, --help              help for helm

Use \"helm [command] --help\" for more information about a command.
";

    #[test]
    fn parses_available_commands() {
        let cmds = parse_helm_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 5); // create/dependency/get/install/repo; Flags ignored
        assert_eq!(
            cmds[0],
            HelmCommand { name: "create".into(), desc: "create a new chart with the given name".into(), section: "Available Commands".into() }
        );
        assert_eq!(cmds.last().unwrap().name, "repo");
        // Flags rows (`--burst-limit`, `-h`) are not commands; the tagline isn't either.
        assert!(cmds.iter().all(|c| !c.name.starts_with('-') && c.name != "helm"));
    }

    #[test]
    fn parses_subcommand_help() {
        // `helm repo --help` — another cobra `Available Commands:` list.
        let out = "add, list, remove, update, and index chart repositories

Usage:
  helm repo [command]

Available Commands:
  add     add a chart repository
  list    list chart repositories
  update  update information of available charts locally from chart repositories

Flags:
  -h, --help   help for repo
";
        let cmds = parse_helm_help(out).unwrap();
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0], HelmCommand { name: "add".into(), desc: "add a chart repository".into(), section: "Available Commands".into() });
    }

    #[test]
    fn leaf_and_non_helm_are_none() {
        // `helm install --help` has Usage/Flags but no command section.
        let out = "This command installs a chart archive.

Usage:
  helm install [NAME] [CHART] [flags]

Flags:
      --atomic   if set, the installation process deletes the installation on failure
";
        assert!(parse_helm_help(out).is_none());
        assert!(parse_helm_help("").is_none());
        assert!(parse_helm_help("just some text\n").is_none());
    }
}
