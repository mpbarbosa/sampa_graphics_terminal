//! Parse `git status --porcelain -b` into a grouped working-tree model.
//!
//! When the user has typed `git` and presses the enhance shortcut, the bridge runs a
//! read-only `git status --porcelain -b` in the session's cwd and hands its output here.
//! Porcelain v1 is git's *stable, locale-independent* machine format (that's its contract),
//! so it — not the human `git status` text — is the parse target. [`parse_status`] yields
//! the branch line (name, upstream, ahead/behind) plus the changed paths already grouped
//! the way `git status` presents them: staged, unstaged, untracked, conflicted. The
//! frontend only renders. Informational; nothing is composed or run.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: input that isn't
//! porcelain output yields `None`, mirroring the other decorator cores.

use serde::{Deserialize, Serialize};

/// One changed path, in the group it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The porcelain status letter that put it in this group (`M`, `A`, `D`, `R`, `C`,
    /// `T`, `?`), or the raw two-letter code for a conflict (`UU`, `AA`, …).
    pub code: String,
    /// Human label for `code` — "modified", "new file", "both modified", …
    pub label: String,
    pub path: String,
    /// Rename/copy source, when git reported one (`R  old -> new`).
    pub orig: Option<String>,
}

/// The working tree as `git status` presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    /// Branch name, `HEAD (detached)` when not on one.
    pub branch: String,
    /// Upstream ref (`origin/main`), when the branch tracks one.
    pub upstream: Option<String>,
    /// Commits ahead of / behind the upstream (0 when there's no upstream).
    pub ahead: u32,
    pub behind: u32,
    /// True for a branch with no commits yet (porcelain's `## No commits yet on <name>`).
    pub no_commits: bool,
    /// Changes staged for commit (index differs from HEAD).
    pub staged: Vec<Change>,
    /// Changes in the working tree that aren't staged.
    pub unstaged: Vec<Change>,
    pub untracked: Vec<Change>,
    /// Unmerged paths (`UU`, `AA`, `DD`, `AU`, `UA`, `DU`, `UD`).
    pub conflicted: Vec<Change>,
}

impl GitStatus {
    /// Nothing to commit — no entry in any group.
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicted.is_empty()
    }
}

/// Parse `git status --porcelain -b` output. The `## <branch>` header is required — it's
/// what distinguishes porcelain output from arbitrary text — so `None` without it. A clean
/// repo (header only) parses to an empty, non-`None` status: "clean" is a real answer.
pub fn parse_status(output: &str) -> Option<GitStatus> {
    let mut lines = output.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    let branch_field = header.strip_prefix("## ")?;
    let mut status = parse_branch(branch_field);

    for line in lines {
        // `XY <path>`: two status columns, a space, then the path. Anything shorter than
        // that isn't an entry — skip it rather than guess.
        let bytes = line.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            continue;
        }
        let x = line[0..1].chars().next()?;
        let y = line[1..2].chars().next()?;
        let (path, orig) = split_rename(&line[3..]);
        let (path, orig) = (unquote(&path), orig.map(|o| unquote(&o)));

        if x == '?' {
            status.untracked.push(Change {
                code: "?".into(),
                label: "untracked".into(),
                path,
                orig: None,
            });
        } else if let Some(label) = conflict_label(x, y) {
            status.conflicted.push(Change {
                code: format!("{x}{y}"),
                label: label.into(),
                path,
                orig,
            });
        } else {
            // `MM` &co. are two distinct facts about one path (staged edit + a further
            // unstaged edit), so the path appears in both groups — as `git status` shows it.
            if x != ' ' && x != '!' {
                status.staged.push(Change {
                    code: x.to_string(),
                    label: code_label(x).into(),
                    path: path.clone(),
                    orig: orig.clone(),
                });
            }
            if y != ' ' && y != '!' {
                status.unstaged.push(Change {
                    code: y.to_string(),
                    label: code_label(y).into(),
                    path,
                    orig,
                });
            }
        }
    }
    Some(status)
}

/// Parse the `## …` header body: `main`, `main...origin/main [ahead 1, behind 2]`,
/// `HEAD (no branch)`, or `No commits yet on main`.
fn parse_branch(field: &str) -> GitStatus {
    let mut status = GitStatus {
        branch: String::new(),
        upstream: None,
        ahead: 0,
        behind: 0,
        no_commits: false,
        staged: Vec::new(),
        unstaged: Vec::new(),
        untracked: Vec::new(),
        conflicted: Vec::new(),
    };

    if field.starts_with("HEAD (no branch)") {
        status.branch = "HEAD (detached)".into();
        return status;
    }

    let mut rest = field;
    if let Some(r) = rest.strip_prefix("No commits yet on ") {
        status.no_commits = true;
        rest = r;
    }

    // The divergence suffix, when present, is the bracketed tail.
    if let Some(open) = rest.rfind(" [") {
        if rest.ends_with(']') {
            let track = &rest[open + 2..rest.len() - 1];
            status.ahead = count_after(track, "ahead ");
            status.behind = count_after(track, "behind ");
            rest = &rest[..open];
        }
    }

    match rest.split_once("...") {
        Some((branch, upstream)) => {
            status.branch = branch.trim().to_string();
            status.upstream = Some(upstream.trim().to_string());
        }
        None => status.branch = rest.trim().to_string(),
    }
    status
}

/// The number following `key` in git's `[ahead 1, behind 2]` tail; 0 if absent.
fn count_after(track: &str, key: &str) -> u32 {
    track
        .split_once(key)
        .map(|(_, rest)| rest.chars().take_while(char::is_ascii_digit).collect::<String>())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Split a rename/copy entry (`old -> new`) into (new path, Some(old path)).
fn split_rename(field: &str) -> (String, Option<String>) {
    match field.split_once(" -> ") {
        Some((orig, path)) => (path.to_string(), Some(orig.to_string())),
        None => (field.to_string(), None),
    }
}

/// Git C-quotes paths with unusual bytes (`"a\tb"`). Drop the wrapping quotes so the path
/// reads naturally; the escapes are left as git wrote them rather than decoded to raw
/// control characters, which have no business reaching the UI.
fn unquote(path: &str) -> String {
    path.strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path)
        .to_string()
}

/// Human label for a single porcelain status letter.
fn code_label(c: char) -> &'static str {
    match c {
        'M' => "modified",
        'A' => "new file",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "typechange",
        'U' => "unmerged",
        '?' => "untracked",
        _ => "changed",
    }
}

/// The unmerged (conflict) code pairs and their `git status` wording; `None` if the pair
/// isn't a conflict.
fn conflict_label(x: char, y: char) -> Option<&'static str> {
    Some(match (x, y) {
        ('D', 'D') => "both deleted",
        ('A', 'U') => "added by us",
        ('U', 'D') => "deleted by them",
        ('U', 'A') => "added by them",
        ('D', 'U') => "deleted by us",
        ('A', 'A') => "both added",
        ('U', 'U') => "both modified",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "## feat/x...origin/feat/x [ahead 2, behind 1]
 M src/main.ts
M  src/style.css
MM crates/gitdec/src/lib.rs
A  crates/gitdec/Cargo.toml
 D docs/old.md
R  a.txt -> c.txt
UU merge.txt
?? new.txt
";

    #[test]
    fn parses_branch_and_divergence() {
        let s = parse_status(SAMPLE).unwrap();
        assert_eq!(s.branch, "feat/x");
        assert_eq!(s.upstream.as_deref(), Some("origin/feat/x"));
        assert_eq!((s.ahead, s.behind), (2, 1));
        assert!(!s.no_commits);
        assert!(!s.is_clean());
    }

    #[test]
    fn groups_entries_the_way_git_status_does() {
        let s = parse_status(SAMPLE).unwrap();
        let paths = |v: &[Change]| v.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        assert_eq!(
            paths(&s.staged),
            [
                "src/style.css",
                "crates/gitdec/src/lib.rs",
                "crates/gitdec/Cargo.toml",
                "c.txt"
            ]
        );
        // `MM` is staged *and* unstaged — one path, two facts.
        assert_eq!(
            paths(&s.unstaged),
            ["src/main.ts", "crates/gitdec/src/lib.rs", "docs/old.md"]
        );
        assert_eq!(paths(&s.untracked), ["new.txt"]);
        assert_eq!(paths(&s.conflicted), ["merge.txt"]);
    }

    #[test]
    fn labels_and_rename_source() {
        let s = parse_status(SAMPLE).unwrap();
        let rename = s.staged.iter().find(|c| c.path == "c.txt").unwrap();
        assert_eq!(rename.code, "R");
        assert_eq!(rename.label, "renamed");
        assert_eq!(rename.orig.as_deref(), Some("a.txt"));
        assert_eq!(s.conflicted[0].code, "UU");
        assert_eq!(s.conflicted[0].label, "both modified");
        assert_eq!(s.untracked[0].label, "untracked");
        assert_eq!(s.staged[2].label, "new file"); // A
        assert_eq!(s.unstaged[2].label, "deleted"); // ' D'
    }

    #[test]
    fn clean_repo_is_a_real_answer_not_none() {
        let s = parse_status("## main...origin/main\n").unwrap();
        assert!(s.is_clean());
        assert_eq!((s.ahead, s.behind), (0, 0));
    }

    #[test]
    fn branch_without_upstream_or_commits() {
        let s = parse_status("## main\n?? a\n").unwrap();
        assert_eq!(s.branch, "main");
        assert!(s.upstream.is_none());

        let fresh = parse_status("## No commits yet on main\n?? a\n").unwrap();
        assert_eq!(fresh.branch, "main");
        assert!(fresh.no_commits);
    }

    #[test]
    fn detached_head() {
        let s = parse_status("## HEAD (no branch)\n M a\n").unwrap();
        assert_eq!(s.branch, "HEAD (detached)");
        assert!(s.upstream.is_none());
    }

    #[test]
    fn quoted_path_loses_only_the_quotes() {
        let s = parse_status("## main\n?? \"odd name.txt\"\n").unwrap();
        assert_eq!(s.untracked[0].path, "odd name.txt");
    }

    #[test]
    fn fails_safe_on_non_porcelain_input() {
        assert!(parse_status("").is_none());
        assert!(parse_status("On branch main\nnothing to commit\n").is_none());
        assert!(parse_status("fatal: not a git repository\n").is_none());
        // A `##`-looking line that isn't the porcelain header shape.
        assert!(parse_status("##main\n").is_none());
    }

    #[test]
    fn malformed_rows_are_skipped_not_guessed() {
        let s = parse_status("## main\nMM\ngarbage\n M ok.txt\n").unwrap();
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.unstaged[0].path, "ok.txt");
    }
}
