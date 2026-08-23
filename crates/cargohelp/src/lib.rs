//! Parse `cargo --help` into a command cheat-sheet for the cargo helper panel.
//!
//! Like `gh`, `cargo` has many subcommands; when the user has typed `cargo` and presses the
//! man-panel shortcut, Sampa shows this list instead of `man cargo`. The bridge runs
//! `cargo --help` and hands its output here; `parse_cargo_help` returns the `name → desc`
//! entries under the single `Commands:` section, in order. Choosing/using an entry is the
//! frontend's job — this crate only parses.
//!
//! cargo's help shape differs from gh's (hence a separate core): one `Commands:` header, and
//! rows are `    <name>[, <alias>]   <description>` — the command plus optional short alias,
//! then two-or-more spaces, then the description. The trailing `    ...   See all commands…`
//! row is skipped. Non-command sections (Options, Usage) are ignored.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: output with no command
//! entries yields `None` (a leaf like `cargo build --help`), mirroring the other cores.

use serde::{Deserialize, Serialize};

/// One `cargo` subcommand: its name (with any alias as printed, e.g. `"build, b"`) and its
/// one-line description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CargoCommand {
    pub name: String,
    pub desc: String,
}

/// Parse `cargo --help` output into its command entries. The `Commands:` header (a
/// non-indented line ending in `:`) opens the section; indented `name   description` rows
/// follow until a blank line or a dedent. The `...` continuation row is skipped. `None` if no
/// entries are found.
pub fn parse_cargo_help(output: &str) -> Option<Vec<CargoCommand>> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in output.lines() {
        let is_header = !line.is_empty() && !line.starts_with(char::is_whitespace);
        if is_header {
            // A new non-indented header. `Commands:` opens the section; anything else ends it.
            in_section = line.trim() == "Commands:";
            continue;
        }
        if !in_section {
            continue;
        }
        // Inside Commands: an indented `name[, alias]   description` row. A blank line ends it.
        if line.trim().is_empty() {
            in_section = false;
            continue;
        }
        let row = line.trim();
        // Split on the first run of 2+ spaces: left = name(+alias), right = description.
        if let Some((name, desc)) = split_columns(row) {
            let name = name.trim();
            let desc = desc.trim();
            // Skip the `...  See all commands with --list` continuation row and any junk.
            if name == "..." || name.is_empty() || desc.is_empty() || name.starts_with('-') {
                continue;
            }
            out.push(CargoCommand { name: name.to_string(), desc: desc.to_string() });
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
            let left = &row[..i];
            let right = row[i..].trim_start();
            return Some((left, right));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Rust's package manager

Usage: cargo [+toolchain] [OPTIONS] [COMMAND]

Options:
  -V, --version  Print version info and exit
  -h, --help     Print help

Commands:
    build, b    Compile the current package
    check, c    Analyze the current package and report errors, but don't build object files
    clean       Remove the target directory
    run, r      Run a binary or example of the local package
    test, t     Run the tests
    ...         See all commands with --list

See 'cargo help <command>' for more information on a specific command.
";

    #[test]
    fn parses_commands_with_aliases() {
        let cmds = parse_cargo_help(SAMPLE).unwrap();
        assert_eq!(cmds.len(), 5); // build/check/clean/run/test; `...` and Options ignored
        assert_eq!(
            cmds[0],
            CargoCommand { name: "build, b".into(), desc: "Compile the current package".into() }
        );
        assert_eq!(cmds[2], CargoCommand { name: "clean".into(), desc: "Remove the target directory".into() });
        assert_eq!(cmds[4].name, "test, t");
        // Options rows (`-V, --version`) are not commands.
        assert!(cmds.iter().all(|c| !c.name.starts_with('-')));
        // The `...` continuation row is dropped.
        assert!(cmds.iter().all(|c| c.name != "..."));
    }

    #[test]
    fn leaf_help_is_none() {
        // `cargo build --help` has Usage/Options but no `Commands:` section.
        let out = "Compile a local package and all of its dependencies

Usage: cargo build [OPTIONS]

Options:
      --release   Build artifacts in release mode
  -h, --help      Print help
";
        assert!(parse_cargo_help(out).is_none());
    }

    #[test]
    fn non_cargo_help_is_none() {
        assert!(parse_cargo_help("").is_none());
        assert!(parse_cargo_help("just some text\nwith no commands\n").is_none());
    }
}
