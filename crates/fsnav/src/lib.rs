//! Read-only directory navigation for the `cd` tree picker.
//!
//! When the user has typed `cd` and presses the enhance shortcut, the frontend opens a
//! tree rooted at the session cwd. This crate lists the **immediate subdirectories** of a
//! path so the tree can be built and expanded **lazily** (one level per expand), and it
//! computes a compact path relative to the root for the inserted `cd` argument.
//!
//! Pure `std::fs` behind serde-able types — **no shell, no Tauri**. The chosen directory
//! is inserted at the prompt by the caller and **never executed** (the insert-never-run
//! boundary the palette/suggester honor). Reading directories the user can already read is
//! not a new capability; nothing here writes or spawns.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// One subdirectory: its display name and absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dir {
    pub name: String,
    pub path: String,
}

/// Immediate subdirectories of `path`, sorted case-insensitively by name. Symlinks that
/// point at directories are followed (so they're navigable). Best-effort: an unreadable or
/// missing `path` yields an empty list, and individual entries that error are skipped.
/// Directories only — files are omitted.
pub fn list_subdirs(path: &str) -> Vec<Dir> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue; // `is_dir` follows symlinks (stat), so symlinked dirs are included
        }
        out.push(Dir {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: p.to_string_lossy().into_owned(),
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// `child` expressed relative to `root`, for a compact `cd` argument (e.g. `src/app`).
/// Returns `"."` when they are the same path, and the unchanged (absolute) `child` when it
/// is not under `root`. Both are expected to be absolute.
pub fn relativize(root: &str, child: &str) -> String {
    match Path::new(child).strip_prefix(Path::new(root)) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            if s.is_empty() {
                ".".to_string()
            } else {
                s.into_owned()
            }
        }
        Err(_) => child.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // A unique temp dir for this test process; cleaned before use.
    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sampa-fsnav-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_only_subdirs_sorted_case_insensitively() {
        let d = tmp("list");
        fs::create_dir(d.join("Beta")).unwrap();
        fs::create_dir(d.join("alpha")).unwrap();
        fs::create_dir(d.join(".hidden")).unwrap();
        fs::write(d.join("afile.txt"), b"x").unwrap(); // a file — must be excluded
        let names: Vec<String> =
            list_subdirs(d.to_str().unwrap()).into_iter().map(|x| x.name).collect();
        // '.' sorts before letters; case-insensitive puts alpha before Beta; file excluded.
        assert_eq!(names, vec![".hidden", "alpha", "Beta"]);
        // Paths are absolute and point back at the entries.
        let dirs = list_subdirs(d.to_str().unwrap());
        assert!(dirs.iter().all(|x| x.path.ends_with(&x.name)));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn missing_or_unreadable_path_is_empty() {
        assert!(list_subdirs("/no/such/path/here-xyz").is_empty());
    }

    #[test]
    fn relativize_cases() {
        assert_eq!(relativize("/home/x", "/home/x/proj/src"), "proj/src");
        assert_eq!(relativize("/home/x", "/home/x"), ".");
        assert_eq!(relativize("/home/x", "/etc/foo"), "/etc/foo"); // not under root → absolute
    }
}
