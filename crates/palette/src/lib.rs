//! Command-palette data (DESIGN.md §10.1): enumerate runnable commands from a PATH
//! string so the palette can offer them. Headless and std-only; the fuzzy filtering
//! and UX live in the frontend.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Runnable command names found on `path` (a `:`-separated PATH string): regular
/// files (symlinks followed) with an execute bit. Names are deduped and sorted — for
/// the palette we only need the set of names, not which directory shadowed which.
pub fn list_executables(path: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // missing / unreadable PATH entry
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if is_executable(&entry.path()) {
                    set.insert(name.to_string());
                }
            }
        }
    }
    set.into_iter().collect()
}

/// True if `path` resolves (following symlinks) to a regular file with any exec bit.
fn is_executable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false, // broken symlink, permission error, etc.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("sampa-palette-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(dir: &Path, name: &str, mode: u32) {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn lists_executables_only() {
        let d = TempDir::new("exec");
        write_file(d.path(), "zzcmd", 0o755);
        write_file(d.path(), "notes.txt", 0o644);
        let list = list_executables(d.path().to_str().unwrap());
        assert!(list.contains(&"zzcmd".to_string()));
        assert!(!list.contains(&"notes.txt".to_string()));
    }

    #[test]
    fn dedupes_and_sorts_across_dirs() {
        let a = TempDir::new("a");
        let b = TempDir::new("b");
        write_file(a.path(), "bcmd", 0o755);
        write_file(a.path(), "acmd", 0o755);
        write_file(b.path(), "bcmd", 0o755); // duplicate name across PATH dirs
        write_file(b.path(), "ccmd", 0o755);
        let path = format!("{}:{}", a.path().display(), b.path().display());
        assert_eq!(list_executables(&path), vec!["acmd", "bcmd", "ccmd"]);
    }

    #[test]
    fn follows_symlinks() {
        let d = TempDir::new("sym");
        write_file(d.path(), "real", 0o755);
        symlink(d.path().join("real"), d.path().join("linked")).unwrap();
        assert!(list_executables(d.path().to_str().unwrap()).contains(&"linked".to_string()));
    }

    #[test]
    fn missing_and_empty_path_entries_skipped() {
        assert!(list_executables("/no/such/dir::").is_empty());
        assert!(list_executables("").is_empty());
    }
}
