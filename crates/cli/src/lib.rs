//! Command-line argument parsing for the Sampa terminal (DESIGN.md §12.2).
//!
//! Implements the conventions launchers and apps rely on so Sampa behaves like a
//! normal terminal: `-e`/`--` to run a command instead of the shell,
//! `--working-directory`, `--title`, `--hold`, `--class`, plus `--config`.
//!
//! Parsing is **infallible** — a GUI app shouldn't crash on a stray flag — so
//! unrecognized tokens are collected into [`CliArgs::warnings`] (the binary logs
//! them) rather than returning an error.

/// Parsed command line. All fields default to "unset".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliArgs {
    /// Command to run instead of the shell (`-e CMD ARGS…` or `-- CMD ARGS…`).
    /// Everything after the flag is taken verbatim as the argv.
    pub exec: Option<Vec<String>>,
    /// Working directory for the first session (`--working-directory DIR` / `-w`).
    pub working_directory: Option<String>,
    /// Window title (`--title STR` / `-T`).
    pub title: Option<String>,
    /// X11 `WM_CLASS` (`--class STR`).
    pub class: Option<String>,
    /// Config file override (`--config FILE`).
    pub config: Option<String>,
    /// Keep the window open after the command exits (`--hold`).
    pub hold: bool,
    /// Start the shell as a login shell (`--login` / `-l`).
    pub login: bool,
    /// `-h` / `--help` was requested.
    pub help: bool,
    /// `-V` / `--version` was requested.
    pub version: bool,
    /// Non-fatal parse problems (unknown flags, missing values).
    pub warnings: Vec<String>,
}

/// `--help` text.
pub const HELP: &str = "\
sampa — a graphical terminal

USAGE:
    sampa [OPTIONS]
    sampa -e COMMAND [ARGS...]
    sampa -- COMMAND [ARGS...]

OPTIONS:
    -e, --command CMD...        Run CMD instead of the shell (consumes the rest)
        --working-directory DIR, -w DIR
                                Start in DIR
    -T, --title STR             Set the window title
        --class STR             Set the X11 WM_CLASS
        --hold                  Keep the window open after the command exits
    -l, --login                 Start the shell as a login shell
        --config FILE           Use FILE instead of the default config
    -h, --help                  Print this help
    -V, --version               Print version
";

/// Pull the value for an option: `--flag=value` (already split into `inline`),
/// otherwise the next argument, advancing `i` past it. Returns `None` if missing.
fn take_value(args: &[String], i: &mut usize, inline: &Option<String>) -> Option<String> {
    if let Some(v) = inline {
        return Some(v.clone());
    }
    if *i + 1 < args.len() {
        *i += 1;
        return Some(args[*i].clone());
    }
    None
}

/// Parse `args` (which must NOT include argv0).
pub fn parse(args: &[String]) -> CliArgs {
    let mut out = CliArgs::default();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // `-e` / `--` (and `--command`): everything after is the command argv.
        if arg == "-e" || arg == "--command" || arg == "--" {
            let rest: Vec<String> = args[i + 1..].to_vec();
            if rest.is_empty() {
                out.warnings.push(format!("{arg} requires a command"));
            } else {
                out.exec = Some(rest);
            }
            break;
        }

        // Split `--flag=value` (only for long flags).
        let (key, inline): (String, Option<String>) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };

        match key.as_str() {
            "--hold" => out.hold = true,
            "--login" | "-l" => out.login = true,
            "--help" | "-h" => out.help = true,
            "--version" | "-V" => out.version = true,
            "--working-directory" | "-w" => {
                out.working_directory = take_value(args, &mut i, &inline);
            }
            "--title" | "-T" => out.title = take_value(args, &mut i, &inline),
            "--class" => out.class = take_value(args, &mut i, &inline),
            "--config" => out.config = take_value(args, &mut i, &inline),
            other => out.warnings.push(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> CliArgs {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn empty_is_default() {
        assert_eq!(parse_str(&[]), CliArgs::default());
    }

    #[test]
    fn exec_with_e_consumes_rest() {
        let c = parse_str(&["-e", "htop", "-d", "5"]);
        assert_eq!(c.exec, Some(vec!["htop".into(), "-d".into(), "5".into()]));
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn exec_with_double_dash() {
        let c = parse_str(&["--hold", "--", "ls", "-la"]);
        assert!(c.hold);
        assert_eq!(c.exec, Some(vec!["ls".into(), "-la".into()]));
    }

    #[test]
    fn e_without_command_warns() {
        let c = parse_str(&["-e"]);
        assert_eq!(c.exec, None);
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn value_flags_both_forms() {
        assert_eq!(parse_str(&["--working-directory=/tmp"]).working_directory, Some("/tmp".into()));
        assert_eq!(parse_str(&["--working-directory", "/tmp"]).working_directory, Some("/tmp".into()));
        assert_eq!(parse_str(&["-w", "/srv"]).working_directory, Some("/srv".into()));
        assert_eq!(parse_str(&["--title", "My Term"]).title, Some("My Term".into()));
        assert_eq!(parse_str(&["-T", "X"]).title, Some("X".into()));
        assert_eq!(parse_str(&["--class=Foo"]).class, Some("Foo".into()));
        assert_eq!(parse_str(&["--config", "/c.toml"]).config, Some("/c.toml".into()));
    }

    #[test]
    fn bool_and_help_flags() {
        let c = parse_str(&["--hold", "-l"]);
        assert!(c.hold && c.login);
        assert!(parse_str(&["-h"]).help);
        assert!(parse_str(&["--version"]).version);
    }

    #[test]
    fn combined_realistic() {
        let c = parse_str(&["--hold", "--title", "Build", "-w", "/tmp", "-e", "vim", "a.txt"]);
        assert!(c.hold);
        assert_eq!(c.title, Some("Build".into()));
        assert_eq!(c.working_directory, Some("/tmp".into()));
        assert_eq!(c.exec, Some(vec!["vim".into(), "a.txt".into()]));
    }

    #[test]
    fn unknown_flag_warns_but_keeps_going() {
        let c = parse_str(&["--nope", "--hold"]);
        assert!(c.hold);
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("--nope"));
    }
}
