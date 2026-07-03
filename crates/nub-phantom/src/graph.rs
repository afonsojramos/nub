//! Walk the module graph reachable from a package's PUBLISHED entry points,
//! collecting the bare-specifier occurrences seen along the way.
//!
//! Restricting to reachable files is what keeps a `devDependencies`-only import
//! in a test/example file (never referenced by `exports`/`main`/`bin`) from being
//! mistaken for a phantom: those files are simply never reached. Relative edges
//! are followed (with Node-style extension/index resolution); bare edges become
//! candidate dependencies.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use crate::extract::{Occurrence, extract};
use crate::manifest::{Entry, EntryKind};
use crate::specifier::{self, SpecKind};

/// Provenance bit: reached from the main surface (`main`/`bin`/`exports."."`).
const FROM_MAIN: u8 = 0b01;
/// Provenance bit: reached from a non-`.` `exports` subpath (the adapter surface).
const FROM_SUBPATH: u8 = 0b10;

/// A bare-specifier reference collected from a reachable file.
#[derive(Debug, Clone)]
pub struct Reference {
    /// Package name the specifier resolves to (`@scope/name` or `name`).
    pub package: String,
    /// The raw specifier (kept for the report — shows the exact subpath).
    pub raw: String,
    /// Guarded (try/catch or a conditional branch) at every occurrence collapses
    /// to soft; a single unguarded occurrence makes the package hard.
    pub soft: bool,
    /// Reachable from the main entry surface.
    pub from_main: bool,
    /// Reachable from a non-`.` `exports` subpath — the "consumer opts into
    /// `<pkg>/<subpath>`" adapter surface. A hard phantom reachable ONLY from a
    /// subpath (not main) is the subpath-adapter class.
    pub from_subpath: bool,
}

/// Result of the reachable-module walk.
#[derive(Debug, Default)]
pub struct Walk {
    pub references: Vec<Reference>,
    pub files_analyzed: usize,
    /// Relative imports that could not be resolved to a file on disk (a tell of
    /// an incomplete tarball or an exotic resolver condition; reported, not fatal).
    pub unresolved_relative: usize,
}

/// Cap the walk so a pathological package (thousands of files) can't stall a
/// scan. Real published entry graphs are far smaller.
const MAX_FILES: usize = 6000;

/// Walk from `entry_points`, following relative edges and collecting bare
/// references. Each reachable file accumulates a provenance mask (which entry
/// surface(s) reach it); a bare reference inherits its file's final mask, so the
/// report can separate subpath-adapter phantoms from main-graph ones.
///
/// Two phases keep provenance correct across diamonds: (1) BFS parses each file
/// once (cached) and propagates provenance bits to fixpoint — a file re-reached
/// with new bits is re-queued for propagation only, never re-parsed; (2) build
/// references from the cache using each file's FINAL mask.
pub fn walk(root: &Path, entry_points: &[Entry]) -> Walk {
    let mut result = Walk::default();
    let mut parsed: BTreeMap<PathBuf, Vec<Occurrence>> = BTreeMap::new();
    let mut flags: BTreeMap<PathBuf, u8> = BTreeMap::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();

    for ep in entry_points {
        if let Some(resolved) = resolve(root, root, &ep.path) {
            let bit = match ep.kind {
                EntryKind::Main => FROM_MAIN,
                EntryKind::Subpath => FROM_SUBPATH,
            };
            if add_flags(&mut flags, &resolved, bit) {
                queue.push_back(resolved);
            }
        }
    }

    while let Some(file) = queue.pop_front() {
        let fflags = *flags.get(&file).unwrap_or(&0);
        // Parse once; a re-queue for provenance propagation reuses the cache.
        if !parsed.contains_key(&file) {
            if parsed.len() >= MAX_FILES {
                continue;
            }
            let Ok(source) = fs::read_to_string(&file) else {
                continue;
            };
            let rel = file.strip_prefix(root).unwrap_or(&file).to_string_lossy();
            parsed.insert(file.clone(), extract(&rel, &source));
        }
        let from_dir = file.parent().unwrap_or(root).to_path_buf();
        // Collect relative edges first (immutable borrow of the cache) then
        // propagate — avoids holding a borrow across the flags mutation.
        let mut targets = Vec::new();
        for occ in &parsed[&file] {
            if let SpecKind::Relative = specifier::classify(&occ.spec) {
                match resolve(root, &from_dir, &occ.spec) {
                    Some(t) => targets.push(t),
                    None => result.unresolved_relative += 1,
                }
            }
        }
        // `ImportsHash` (self) and `NonPackage` (URL/virtual/internal) are not
        // dependency edges; only `Bare` references are collected in phase 2.
        for t in targets {
            if add_flags(&mut flags, &t, fflags) {
                queue.push_back(t);
            }
        }
    }

    result.files_analyzed = parsed.len();
    for (file, occs) in &parsed {
        let fflags = *flags.get(file).unwrap_or(&0);
        for occ in occs {
            if let SpecKind::Bare(package) = specifier::classify(&occ.spec) {
                result.references.push(Reference {
                    package,
                    raw: occ.spec.clone(),
                    soft: occ.soft,
                    from_main: fflags & FROM_MAIN != 0,
                    from_subpath: fflags & FROM_SUBPATH != 0,
                });
            }
        }
    }
    result
}

/// OR `bit` into `path`'s provenance mask. Returns true if the mask GREW (new
/// file, or new bits) — the caller then (re)queues it so the new provenance
/// propagates to its edges. The 2-bit lattice bounds re-queues to ≤2 per file.
fn add_flags(flags: &mut BTreeMap<PathBuf, u8>, path: &Path, bit: u8) -> bool {
    let entry = flags.entry(path.to_path_buf()).or_insert(0);
    let before = *entry;
    *entry |= bit;
    *entry != before
}

/// Node-style resolution of a relative specifier to a concrete JS file under
/// `root`. Tries the literal path, then extension suffixes, then `index.*`, then
/// a directory's `package.json` `main`. Returns `None` (and does not escape
/// `root`) if nothing resolves to a real JS-like file.
fn resolve(root: &Path, from_dir: &Path, spec: &str) -> Option<PathBuf> {
    let joined = from_dir.join(spec);
    // Keep the walk inside the package tree (a `../../` that climbs out is not
    // part of the published surface).
    let base = normalize(&joined);
    if !base.starts_with(root) {
        return None;
    }

    let exts = ["js", "cjs", "mjs", "jsx", "ts", "tsx", "mts", "cts"];

    // 1. Exact file.
    if is_js_file(&base) {
        return Some(base);
    }
    // 2. `base.<ext>`.
    for ext in exts {
        let cand = with_appended_ext(&base, ext);
        if is_js_file(&cand) {
            return Some(cand);
        }
    }
    // 3. `base/index.<ext>`.
    for ext in exts {
        let cand = base.join(format!("index.{ext}"));
        if is_js_file(&cand) {
            return Some(cand);
        }
    }
    // 4. `base/package.json` → its `main`.
    let pkg = base.join("package.json");
    if let Ok(raw) = fs::read(&pkg) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw)
            && let Some(main) = v.get("main").and_then(|m| m.as_str())
        {
            return resolve(root, &base, main);
        }
        // package.json with no main → default index.js in that dir.
        for ext in exts {
            let cand = base.join(format!("index.{ext}"));
            if is_js_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// Append an extension to a path's file name (`a/b` + `js` → `a/b.js`), rather
/// than replacing an existing one (`a/b.min` must become `a/b.min.js`).
fn with_appended_ext(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

fn is_js_file(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    crate::manifest::is_js_like(name)
}

/// Lexically normalize `.`/`..` segments WITHOUT touching the filesystem (we do
/// not want symlink resolution; a tarball has none anyway). Used only to enforce
/// the stay-under-root invariant.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        use std::path::Component::*;
        match comp {
            ParentDir => {
                out.pop();
            }
            CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::walk;
    use crate::manifest::{Entry, EntryKind};
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nub-phantom-graph-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn main_entry(path: &str) -> Entry {
        Entry {
            path: path.to_string(),
            kind: EntryKind::Main,
        }
    }

    #[test]
    fn follows_relative_edges_collects_bare_ignores_unreached() {
        let root = scratch("reach");
        // entry → ./util (reached) imports "declared-dep"; orphan test file
        // imports "dev-only" but is never referenced.
        fs::write(
            root.join("index.js"),
            "require('./util'); require('real-dep');",
        )
        .unwrap();
        fs::write(root.join("util.js"), "import x from 'util-dep';").unwrap();
        fs::write(root.join("test.js"), "require('dev-only');").unwrap();

        let w = walk(&root, &[main_entry("index.js")]);
        let pkgs: Vec<_> = w.references.iter().map(|r| r.package.as_str()).collect();
        assert!(pkgs.contains(&"real-dep"));
        assert!(pkgs.contains(&"util-dep"));
        assert!(
            !pkgs.contains(&"dev-only"),
            "unreached test file must not contribute"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn subpath_provenance_separates_adapter_from_main_graph() {
        let root = scratch("subpath");
        // main graph imports `main-dep`; the `./zod` adapter subpath imports
        // `backend-zod`. The zod import must carry from_subpath && !from_main.
        fs::write(root.join("index.js"), "require('main-dep');").unwrap();
        fs::write(root.join("zod.js"), "import 'backend-zod';").unwrap();

        let w = walk(
            &root,
            &[
                main_entry("index.js"),
                Entry {
                    path: "zod.js".to_string(),
                    kind: EntryKind::Subpath,
                },
            ],
        );
        let zod = w
            .references
            .iter()
            .find(|r| r.package == "backend-zod")
            .unwrap();
        assert!(zod.from_subpath && !zod.from_main, "adapter-only backend");
        let main = w
            .references
            .iter()
            .find(|r| r.package == "main-dep")
            .unwrap();
        assert!(main.from_main && !main.from_subpath, "main-graph dep");
        let _ = fs::remove_dir_all(&root);
    }
}
