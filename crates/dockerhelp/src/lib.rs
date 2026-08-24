//! Parse `docker --help` into a grouped command cheat-sheet for the docker helper panel.
//!
//! Like `gh`/`cargo`/`npm`, `docker` has many subcommands; when the user has typed `docker` and
//! presses the man-panel shortcut, Sampa shows this list instead of a man page. The bridge runs
//! `docker --help` and hands its output here; `parse_docker_help` returns the `name → desc`
//! entries under each `… Commands:` section, in order.
//!
//! docker's help shape is a blend of gh's and cargo's (hence a separate core): it **groups**
//! commands under several headers (`Common Commands:`, `Management Commands:`, `Swarm
//! Commands:`, `Commands:`) like gh, but each row is `  <name>  <description>` (space-separated,
//! not `name: desc`) like cargo. Plugin commands are printed with a trailing `*` (`buildx*`,
//! `compose*`) — the star is stripped from the stored name. Non-command sections (Global
//! Options, Usage) end the current section so their lines are ignored.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no command
//! entries yields `None` (a leaf like `docker run --help`), mirroring the other help cores.

use serde::{Deserialize, Serialize};

/// One `docker` subcommand: its name, one-line description, and the section it appeared under
/// (e.g. `"Common Commands"`, `"Management Commands"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerCommand {
    pub name: String,
    pub desc: String,
    pub section: String,
}

/// Parse `docker --help` output into its command entries. A section header is a non-indented
/// line ending in `Commands:`; the stored section is that line minus the trailing colon. Rows
/// under it are indented `name  description` lines (split on the first run of 2+ spaces). Any
/// other non-indented line (Usage, Global Options, the tagline) ends the current section.
/// `None` if no entries are found.
pub fn parse_docker_help(output: &str) -> Option<Vec<DockerCommand>> {
    let mut section: Option<String> = None;
    let mut out = Vec::new();
    for line in output.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            let h = line.trim();
            section = h
                .strip_suffix("Commands:")
                .map(|_| h.trim_end_matches(':').to_string());
            continue;
        }
        let Some(sec) = &section else { continue };
        if line.trim().is_empty() {
            continue; // a blank line does not end the section; the next header does
        }
        if let Some((name, desc)) = split_columns(line.trim()) {
            // Plugin commands print a trailing `*` (buildx*, compose*) — drop it.
            let name = name.trim().trim_end_matches('*');
            let desc = desc.trim();
            if !name.is_empty()
                && !desc.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.push(DockerCommand {
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

    const SAMPLE: &str = "Usage:  docker [OPTIONS] COMMAND

A self-sufficient runtime for containers

Common Commands:
  run         Create and run a new container from an image
  ps          List containers
  build       Build an image from a Dockerfile

Management Commands:
  builder     Manage builds
  buildx*     Docker Buildx
  container   Manage containers

Swarm Commands:
  swarm       Manage Swarm

Global Options:
      --config string      Location of client config files
";

    #[test]
    fn parses_grouped_commands() {
        let cmds = parse_docker_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 7); // 3 common + 3 management + 1 swarm; Global Options ignored
        assert_eq!(
            cmds[0],
            DockerCommand { name: "run".into(), desc: "Create and run a new container from an image".into(), section: "Common Commands".into() }
        );
        assert_eq!(cmds[3].section, "Management Commands");
        // The plugin star is stripped: `buildx*` → `buildx`.
        assert!(cmds.iter().any(|c| c.name == "buildx"));
        assert!(cmds.iter().all(|c| !c.name.ends_with('*')));
        assert_eq!(cmds.last().unwrap(), &DockerCommand { name: "swarm".into(), desc: "Manage Swarm".into(), section: "Swarm Commands".into() });
        // The `--config` flag under Global Options is not a command.
        assert!(cmds.iter().all(|c| !c.name.starts_with('-')));
    }

    #[test]
    fn subcommand_help_is_grouped_too() {
        // `docker container --help` uses a single `Commands:` section.
        let out = "Usage:  docker container COMMAND

Manage containers

Commands:
  ls          List containers
  rm          Remove one or more containers

Run 'docker container COMMAND --help' for more information.
";
        let cmds = parse_docker_help(out).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], DockerCommand { name: "ls".into(), desc: "List containers".into(), section: "Commands".into() });
    }

    #[test]
    fn leaf_and_non_docker_are_none() {
        // `docker run --help` has Usage/Options but no `… Commands:` section.
        let out = "Usage:  docker run [OPTIONS] IMAGE

Create and run a new container from an image

Options:
  -d, --detach    Run container in background
";
        assert!(parse_docker_help(out).is_none());
        assert!(parse_docker_help("").is_none());
        assert!(parse_docker_help("just some text\n").is_none());
    }
}
