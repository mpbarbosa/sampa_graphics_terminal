//! Parse `npm --help` into the command-name list for the npm helper panel.
//!
//! Like `gh` and `cargo`, `npm` has many subcommands; when the user has typed `npm` and presses
//! the man-panel shortcut, Sampa shows this list instead of the pager. The bridge runs
//! `npm --help` and hands its output here; `parse_npm_help` returns the command names under the
//! `All commands:` header, in order.
//!
//! npm's help shape differs from gh's and cargo's (hence a separate core): it has **no
//! per-command descriptions** — the `All commands:` section is a comma-separated, line-wrapped
//! list of bare names (`access, approve-scripts, audit, …`). So this core returns names only;
//! the frontend lays them out in columns.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no `All commands:`
//! list yields `None` (a leaf like `npm install --help`, or an npm too old to use this format),
//! mirroring the other help cores.

/// Parse `npm --help` output into its command names. The `All commands:` header (a non-indented
/// line) opens the section; the indented, comma-separated names that follow are collected until
/// a blank line or a dedent. `None` if no names are found.
pub fn parse_npm_help(output: &str) -> Option<Vec<String>> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in output.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            // `All commands:` opens the list; any other non-indented line ends it.
            in_section = line.trim() == "All commands:";
            continue;
        }
        if !in_section {
            continue;
        }
        // Skip blank lines within the section (npm prints one right after `All commands:`, and
        // may wrap the list across paragraphs). The section ends at the next non-indented
        // header, handled above — not on a blank line.
        if line.trim().is_empty() {
            continue;
        }
        for tok in line.split(',') {
            let name = tok.trim();
            // A command name: lowercase letters, digits, and dashes (e.g. `install-ci-test`).
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.push(name.to_string());
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "npm <command>

Usage:

npm install        install all the dependencies in your project
npm test           run this project's tests

All commands:

    access, approve-scripts, audit, bugs, cache, ci,
    completion, config, dedupe, install-ci-test, run,
    test, version, view, whoami

Specify configs in the ini-formatted file:
    /home/me/.npmrc
";

    #[test]
    fn parses_command_names() {
        let cmds = parse_npm_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 15);
        assert_eq!(cmds[0], "access");
        assert_eq!(cmds[1], "approve-scripts");
        assert!(cmds.contains(&"install-ci-test".to_string())); // dashed name survives
        assert_eq!(cmds.last().unwrap(), "whoami");
        // The Usage example lines (`npm install …`) are not in the list.
        assert!(!cmds.contains(&"npm".to_string()));
    }

    #[test]
    fn leaf_help_is_none() {
        // `npm install --help` has Usage/Options but no `All commands:` section.
        let out = "Install a package

Usage:
npm install [<package-spec> ...]

Options:
[-g|--global]
";
        assert!(parse_npm_help(out).is_none());
    }

    #[test]
    fn non_npm_help_is_none() {
        assert!(parse_npm_help("").is_none());
        assert!(parse_npm_help("just some text\nwith no commands\n").is_none());
    }
}
