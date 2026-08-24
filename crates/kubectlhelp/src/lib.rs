//! Parse `kubectl --help` into a grouped command cheat-sheet for the kubectl helper panel.
//!
//! Like `docker`, `kubectl` has dozens of subcommands grouped under headers; when the user has
//! typed `kubectl` and presses the man-panel shortcut, Sampa shows this list instead of a man
//! page. The bridge runs `kubectl --help` and hands its output here; `parse_kubectl_help`
//! returns the `name → desc` entries under each command section, in order.
//!
//! kubectl's help is docker-shaped — grouped sections with `  <name>  <description>` rows — but
//! its section headers carry qualifiers, so they don't end in exactly `Commands:`: the top-level
//! help uses `Basic Commands (Beginner):`, `Deploy Commands:`, `Other Commands:`, … and its
//! subcommands (via cobra) use `Available Commands:`. Hence a distinct header rule (a
//! non-indented line that ends in `:` and mentions "command"/"subcommand"), and a separate core
//! from `sampa-dockerhelp`. Non-command sections (Usage, Options, Examples) end the current
//! section so their lines are ignored.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no command
//! entries yields `None` (a leaf like `kubectl get --help`), mirroring the other help cores.

use serde::{Deserialize, Serialize};

/// One `kubectl` subcommand: its name, one-line description, and the section it appeared under
/// (e.g. `"Basic Commands (Beginner)"`, `"Available Commands"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubectlCommand {
    pub name: String,
    pub desc: String,
    pub section: String,
}

/// True for a kubectl command-section header: a non-indented line ending in `:` whose text
/// mentions a command group (`… Commands:`, `Available Commands:`, `Subcommands …:`). Matches
/// the qualified top-level headers and cobra's subcommand header; excludes `Usage:` / `Options:`.
fn is_command_header(header: &str) -> bool {
    header.ends_with(':') && header.to_ascii_lowercase().contains("command")
}

/// Parse `kubectl --help` output into its command entries. A section header is a non-indented
/// line ending in `:` that mentions "command"; the stored section is that line minus the
/// trailing colon. Rows under it are indented `name  description` lines (split on the first run
/// of 2+ spaces). Any other non-indented line (Usage, Options, the tagline) ends the current
/// section. `None` if no entries are found.
pub fn parse_kubectl_help(output: &str) -> Option<Vec<KubectlCommand>> {
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
                out.push(KubectlCommand {
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

    const SAMPLE: &str = "kubectl controls the Kubernetes cluster manager.

 Find more information at: https://kubernetes.io/docs/reference/kubectl/

Basic Commands (Beginner):
  create          Create a resource from a file or from stdin
  run             Run a particular image on the cluster

Deploy Commands:
  rollout         Manage the rollout of a resource
  scale           Set a new size for a deployment, replica set, or replication controller

Other Commands:
  api-resources   Print the supported API resources on the server
  config          Modify kubeconfig files

Usage:
  kubectl [flags] [options]

Use \"kubectl <command> --help\" for more information about a given command.
";

    #[test]
    fn parses_grouped_commands() {
        let cmds = parse_kubectl_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 6); // 2 basic + 2 deploy + 2 other; Usage ignored
        assert_eq!(
            cmds[0],
            KubectlCommand { name: "create".into(), desc: "Create a resource from a file or from stdin".into(), section: "Basic Commands (Beginner)".into() }
        );
        assert_eq!(cmds[2].section, "Deploy Commands");
        // Dashed name survives; the qualified header parsed despite the parenthetical.
        assert!(cmds.iter().any(|c| c.name == "api-resources"));
        assert_eq!(cmds.last().unwrap().section, "Other Commands");
        // The tagline and Usage block are not commands.
        assert!(cmds.iter().all(|c| !c.name.starts_with('-') && c.name != "kubectl"));
    }

    #[test]
    fn parses_cobra_subcommand_help() {
        // `kubectl config --help` (cobra) uses an `Available Commands:` header.
        let out = "Modify kubeconfig files using subcommands like \"kubectl config set current-context\".

Available Commands:
  get-contexts   Describe one or many contexts
  use-context    Set the current-context in a kubeconfig file

Usage:
  kubectl config SUBCOMMAND [options]
";
        let cmds = parse_kubectl_help(out).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], KubectlCommand { name: "get-contexts".into(), desc: "Describe one or many contexts".into(), section: "Available Commands".into() });
    }

    #[test]
    fn leaf_and_non_kubectl_are_none() {
        // `kubectl get --help` has Examples/Options but no command section.
        let out = "Display one or many resources

Examples:
  kubectl get pods

Options:
  -o, --output=''   Output format
";
        assert!(parse_kubectl_help(out).is_none());
        assert!(parse_kubectl_help("").is_none());
        assert!(parse_kubectl_help("just some text\n").is_none());
    }
}
