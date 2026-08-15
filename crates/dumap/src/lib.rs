//! Parse `du(1)` output into a sized directory tree for the disk-usage treemap.
//!
//! When the user has typed `du` and presses the enhance shortcut, the bridge runs a
//! read-only `du -k` on the cwd and hands its output here. `parse_du` turns the flat list
//! of `<size>\t<path>` lines (cumulative KiB, one per directory) into a nested tree with
//! children sorted largest-first, ready for the frontend to lay out as a squarified
//! treemap. The chosen directory is inserted at the prompt (`cd <path>`), never executed.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: malformed input yields
//! `None` (the caller shows nothing), mirroring the `ps-decorate` gate discipline.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A directory in the tree: its display name, full path, cumulative size (KiB, as `du -k`
/// reports — the directory and everything under it), and child directories (largest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuNode {
    pub name: String,
    pub path: String,
    pub size_kb: u64,
    pub children: Vec<DuNode>,
}

/// Parse `du -k` output into a tree rooted at the `du` target. Each line is
/// `<size>\t<path>`; `du` prints children before parents (post-order), sizes are
/// cumulative. `None` if there are no valid rows or any row is malformed (no tab /
/// non-numeric size) — the run-fresh caller controls the format, so this is defensive.
pub fn parse_du(output: &str) -> Option<DuNode> {
    let mut size: HashMap<String, u64> = HashMap::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let (s, p) = line.split_once('\t')?;
        let kb: u64 = s.trim().parse().ok()?;
        let path = normalize(p);
        if !path.is_empty() {
            size.insert(path, kb);
        }
    }
    if size.is_empty() {
        return None;
    }

    // parent → child paths, and the roots (whose parent isn't itself a du entry).
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();
    for path in size.keys() {
        match parent_path(path) {
            Some(par) if size.contains_key(&par) => {
                children_of.entry(par).or_default().push(path.clone());
            }
            _ => roots.push(path.clone()),
        }
    }
    // The target is the largest root (`du .` yields exactly one; be robust if not).
    roots.sort_by_key(|p| std::cmp::Reverse(size[p]));
    let root = roots.into_iter().next()?;
    Some(build(&root, &size, &children_of))
}

fn build(
    path: &str,
    size: &HashMap<String, u64>,
    children_of: &HashMap<String, Vec<String>>,
) -> DuNode {
    let mut children: Vec<DuNode> = children_of
        .get(path)
        .map(|kids| kids.iter().map(|c| build(c, size, children_of)).collect())
        .unwrap_or_default();
    children.sort_by(|a, b| b.size_kb.cmp(&a.size_kb).then_with(|| a.name.cmp(&b.name)));
    DuNode {
        name: base_name(path),
        path: path.to_string(),
        size_kb: size.get(path).copied().unwrap_or(0),
        children,
    }
}

/// Trim a single trailing slash (but keep root `/` intact) and surrounding whitespace.
fn normalize(p: &str) -> String {
    let p = p.trim();
    if p.len() > 1 {
        p.trim_end_matches('/').to_string()
    } else {
        p.to_string()
    }
}

/// The parent path, or `None` for a top-level entry (`.`, a bare name, or `/abs`).
fn parent_path(p: &str) -> Option<String> {
    match p.rsplit_once('/') {
        Some(("", _)) => None, // "/abs" — parent would be "/" root marker, treat as root
        Some((par, _)) => Some(par.to_string()),
        None => None, // "." or "name"
    }
}

/// The last path component (the directory's own name).
fn base_name(p: &str) -> String {
    match p.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name.to_string(),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `du -k .` on a small tree, post-order as du prints it.
    const SAMPLE: &str = "4\t./a/x\n8\t./a\n4\t./b\n16\t.\n";

    #[test]
    fn builds_a_sized_tree() {
        let root = parse_du(SAMPLE).unwrap();
        assert_eq!(root.name, ".");
        assert_eq!(root.size_kb, 16);
        // Children largest-first: a (8) before b (4).
        assert_eq!(root.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(root.children[0].size_kb, 8);
        // Nested child.
        assert_eq!(root.children[0].children[0].name, "x");
        assert_eq!(root.children[0].children[0].size_kb, 4);
        assert!(root.children[1].children.is_empty());
    }

    #[test]
    fn absolute_paths_root_correctly() {
        let out = "100\t/tmp/proj/src\n250\t/tmp/proj\n";
        let root = parse_du(out).unwrap();
        assert_eq!(root.path, "/tmp/proj");
        assert_eq!(root.name, "proj");
        assert_eq!(root.size_kb, 250);
        assert_eq!(root.children[0].path, "/tmp/proj/src");
        assert_eq!(root.children[0].name, "src");
    }

    #[test]
    fn trailing_slashes_and_crlf_tolerated() {
        let out = "8\t./a/\r\n12\t./\r\n";
        let root = parse_du(out).unwrap();
        assert_eq!(root.name, ".");
        assert_eq!(root.size_kb, 12);
        assert_eq!(root.children[0].name, "a");
    }

    #[test]
    fn malformed_or_empty_is_none() {
        assert!(parse_du("").is_none());
        assert!(parse_du("not du output\nat all\n").is_none()); // no tab
        assert!(parse_du("x\t./a\n").is_none()); // non-numeric size
    }
}
