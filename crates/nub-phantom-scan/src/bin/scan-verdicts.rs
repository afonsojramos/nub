//! scan-verdicts — dump the dynamic scanner's per-package verdict over an
//! installed tree, for corpus-alignment spot-checking. Enumerates true
//! node_modules-child package roots (canonicalize-dedup) and prints
//! `name<TAB>has_unguarded_phantom<TAB>target,target,…` one per line.
//!
//! Usage: `scan-verdicts <node_modules-dir>`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: scan-verdicts <node_modules-dir>");
    let mut out: Vec<(String, bool, String)> = Vec::new();
    for root in enumerate(Path::new(&dir)) {
        let name = read_name(&root).unwrap_or_else(|| root.display().to_string());
        match nub_phantom_scan::scan_extracted(&root) {
            Some(r) => {
                let targets: Vec<&str> = r.targets.iter().map(|t| t.name.as_str()).collect();
                out.push((name, r.has_unguarded_phantom, targets.join(",")));
            }
            None => out.push((name, false, "<scan-miss>".into())),
        }
    }
    out.sort();
    out.dedup();
    for (name, flagged, targets) in out {
        println!("{name}\t{flagged}\t{targets}");
    }
}

fn read_name(root: &Path) -> Option<String> {
    let raw = std::fs::read(root.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v.get("name").and_then(|n| n.as_str()).map(str::to_string)
}

fn enumerate(node_modules: &Path) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out = Vec::new();
    let mut stack = vec![node_modules.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            let is_dir = ft.is_dir()
                || (ft.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_dir()));
            if !is_dir {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".bin" || name == ".cache" {
                continue;
            }
            let parent_name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str());
            let is_root = parent_name == Some("node_modules")
                || (parent_name.is_some_and(|n| n.starts_with('@'))
                    && path
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|gp| gp.file_name())
                        .and_then(|n| n.to_str())
                        == Some("node_modules"));
            if is_root
                && path.join("package.json").is_file()
                && let Ok(canon) = std::fs::canonicalize(&path)
                && seen.insert(canon.clone())
            {
                out.push(canon);
            }
            stack.push(path);
        }
    }
    out
}
