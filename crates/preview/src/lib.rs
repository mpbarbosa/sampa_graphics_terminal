//! Safe auto-run preview (DESIGN.md §10.3, §13).
//!
//! As the user types a **syntactically valid, read-only** command, we run it in a
//! throwaway shell in the session's cwd and show the output — without touching the
//! interactive session. This is the highest-risk feature, so the design is
//! safety-first and the gate is **authoritative here in the core**; the frontend can
//! never bypass it.
//!
//! Pipeline:
//!   1. [`classify`] (pure, exhaustively tested) — reject anything that can write,
//!      chain, substitute, redirect, background, or run a non-allowlisted command.
//!   2. [`syntax_ok`] — `zsh -n -c <line>` parses (never executes); incomplete lines
//!      like `ls |` are rejected.
//!   3. execute the survivor with cwd = the session dir, **stdin closed**, a wall-clock
//!      timeout (SIGKILL on expiry), and an output cap.
//!
//! "Read-only" is a conservative allowlist, not a proof: an allowed command can still
//! read files the user could read anyway (it's their own machine). The allowlist is
//! deliberately small.

use std::process::{Command, Stdio};
use std::time::Duration;

/// Wall-clock limit for a preview command.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Max bytes of output shown.
const OUTPUT_CAP: usize = 64 * 1024;

/// Metacharacter fragments that enable writes / side effects / hidden execution.
/// Their mere presence rejects the line (§10.3).
const BANNED: &[&str] = &[
    "`", "$(", "${", "&&", "||", ">>", ">", "<", ";", "&", "|&", "\n",
];

/// Commands considered read-only enough to preview. Default-deny: anything not here
/// is rejected. `git`/`find`/`tail` get extra per-command checks below.
const ALLOWLIST: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "egrep", "fgrep", "echo", "printf", "pwd",
    "date", "whoami", "id", "uname", "hostname", "nproc", "uptime", "wc", "sort",
    "uniq", "cut", "tr", "column", "tree", "stat", "file", "du", "df", "printenv",
    "which", "type", "basename", "dirname", "realpath", "readlink", "ps", "lsblk",
    "free", "git", "tac", "nl", "rev", "fold", "comm", "seq", "find", "cksum",
    "md5sum", "sha256sum", "env",
];

/// Read-only `git` subcommands.
const GIT_READONLY: &[&str] = &[
    "status", "log", "diff", "show", "blame", "describe", "shortlog", "branch",
];

/// `find` actions that execute or delete — reject.
const FIND_DANGEROUS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
    "-fprint0",
];

/// Whether the line is allowed to run as a preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// Not run, with a short human reason.
    Rejected(String),
}

/// The authoritative gate. Pure; no I/O.
pub fn classify(line: &str) -> Verdict {
    let line = line.trim();
    if line.is_empty() {
        return Verdict::Rejected("empty".into());
    }
    for bad in BANNED {
        if line.contains(bad) {
            return Verdict::Rejected(format!("contains `{bad}`"));
        }
    }
    // Each `|` pipeline stage must itself be an allowlisted read-only command.
    for stage in line.split('|') {
        if let Err(reason) = classify_stage(stage.trim()) {
            return Verdict::Rejected(reason);
        }
    }
    Verdict::Allowed
}

fn classify_stage(stage: &str) -> Result<(), String> {
    let tokens: Vec<&str> = stage.split_whitespace().collect();
    let Some(&cmd) = tokens.first() else {
        return Err("empty pipeline stage".into());
    };
    if cmd.contains('=') {
        return Err("environment assignment".into());
    }
    if cmd.contains('/') {
        return Err("path-qualified command".into());
    }
    if !ALLOWLIST.contains(&cmd) {
        return Err(format!("`{cmd}` is not on the read-only allowlist"));
    }
    match cmd {
        "git" => {
            let sub = tokens.get(1).copied().unwrap_or("");
            if !GIT_READONLY.contains(&sub) {
                return Err(format!("`git {sub}` is not read-only"));
            }
        }
        "find" => {
            for &t in &tokens[1..] {
                if FIND_DANGEROUS.contains(&t) {
                    return Err(format!("`find {t}`"));
                }
            }
        }
        "tail" => {
            for &t in &tokens[1..] {
                if t == "-f" || t == "-F" || t == "--follow" || t.starts_with("-f") {
                    return Err("`tail -f` would not terminate".into());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// `zsh -n -c <line>` — does it parse? (Never executes.) Incomplete/invalid → false.
pub fn syntax_ok(line: &str) -> bool {
    Command::new("zsh")
        .args(["-n", "-c", line])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The result of a preview attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// The command ran; here is its (possibly truncated) combined output.
    Ran(String),
    /// Not run — the gate rejected it or it was incomplete. Carries the reason.
    NotRun(String),
}

/// Gate `line` and, if it passes, run it in `cwd` under strict limits (§10.3).
pub fn run_preview(line: &str, cwd: Option<&str>) -> Preview {
    match classify(line) {
        Verdict::Rejected(r) => return Preview::NotRun(r),
        Verdict::Allowed => {}
    }
    if !syntax_ok(line) {
        return Preview::NotRun("incomplete or invalid syntax".into());
    }
    execute(line, cwd)
}

fn execute(line: &str, cwd: Option<&str>) -> Preview {
    let mut cmd = Command::new("zsh");
    cmd.args(["-c", line])
        .stdin(Stdio::null()) // so `cat`/`sort` with no args get EOF, not a hang
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Preview::NotRun(format!("could not start preview: {e}")),
    };
    let pid = child.id();

    // Read output + reap on a worker thread; enforce the timeout by SIGKILL'ing the
    // process group leader if it overruns.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(output)) => {
            let mut bytes = output.stdout;
            bytes.extend_from_slice(&output.stderr);
            let truncated = bytes.len() > OUTPUT_CAP;
            bytes.truncate(OUTPUT_CAP);
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            if truncated {
                text.push_str("\n…(truncated)");
            }
            Preview::Ran(text)
        }
        Ok(Err(e)) => Preview::NotRun(format!("preview failed: {e}")),
        Err(_) => {
            // Timed out: kill it. (The worker thread then completes and is dropped.)
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            Preview::Ran("(preview timed out)".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(line: &str) -> bool {
        classify(line) == Verdict::Allowed
    }

    #[test]
    fn allows_read_only_commands() {
        for ok in [
            "ls", "ls -la", "cat file.txt", "pwd", "date", "echo hi",
            "grep foo bar.txt", "ls -la | grep txt", "head -n5 f | wc -l",
            "git status", "git log --oneline", "git diff", "find . -name '*.rs'",
            "tail -n 20 log", "sort f | uniq -c",
        ] {
            assert!(allowed(ok), "should allow: {ok}");
        }
    }

    #[test]
    fn rejects_writes_and_side_effects() {
        for bad in [
            "rm file", "rm -rf /", "mv a b", "cp a b", "touch x", "mkdir d",
            "dd if=/dev/zero of=x", "tee out", "chmod 777 f", ": > file",
        ] {
            assert!(!allowed(bad), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_metacharacters() {
        for bad in [
            "ls > out", "ls >> out", "cat < in", "ls; rm x", "ls && rm x",
            "ls || rm x", "ls & ", "echo $(rm x)", "echo `rm x`", "echo ${x}",
            "cat f | tee out",
        ] {
            assert!(!allowed(bad), "should reject: {bad}");
        }
    }

    #[test]
    fn git_only_read_only_subcommands() {
        assert!(allowed("git status"));
        assert!(allowed("git log -p"));
        for bad in ["git commit -m x", "git push", "git checkout .", "git reset --hard", "git rm f"] {
            assert!(!allowed(bad), "should reject: {bad}");
        }
    }

    #[test]
    fn find_and_tail_dangerous_flags_rejected() {
        for bad in ["find . -delete", "find . -exec rm {} ;", "find . -execdir rm {} ;"] {
            assert!(!allowed(bad), "should reject: {bad}");
        }
        assert!(!allowed("tail -f log"));
        assert!(!allowed("tail --follow log"));
    }

    #[test]
    fn non_allowlisted_and_path_qualified_rejected() {
        assert!(!allowed("python -c 'x'"));
        assert!(!allowed("perl -e 'x'"));
        assert!(!allowed("/bin/rm file"));
        assert!(!allowed("./script.sh"));
        assert!(!allowed("FOO=bar ls"));
    }

    // The milestone gate (§13): a typed `rm` never runs — filesystem-verified.
    #[test]
    fn typed_rm_never_deletes_the_file() {
        let dir = std::env::temp_dir().join(format!("sampa-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("do-not-delete.txt");
        std::fs::write(&victim, "precious").unwrap();
        let dstr = dir.to_str().unwrap();

        // Every one of these MUST NOT run — the file must survive.
        for line in [
            "rm do-not-delete.txt",
            "rm -f do-not-delete.txt",
            "mv do-not-delete.txt gone",
            "> do-not-delete.txt",
            ": > do-not-delete.txt",
            "echo x > do-not-delete.txt",
            "find . -delete",
        ] {
            let r = run_preview(line, Some(dstr));
            assert!(matches!(r, Preview::NotRun(_)), "{line:?} should be NotRun, got {r:?}");
            assert!(victim.exists(), "{line:?} deleted/clobbered the file!");
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious", "{line:?} modified the file!");
        }

        // A safe read-only command DOES run, in the given cwd.
        match run_preview("cat do-not-delete.txt", Some(dstr)) {
            Preview::Ran(out) => assert!(out.contains("precious"), "cat output: {out:?}"),
            other => panic!("cat should run, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
