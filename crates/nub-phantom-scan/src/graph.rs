//! Walk the module graph reachable from a package's PUBLISHED entry points,
//! collecting the bare-specifier occurrences seen along the way.
//!
//! Restricting to reachable files is what keeps a `devDependencies`-only import
//! in a test/example file (never referenced by `exports`/`main`/`bin`) from being
//! mistaken for a phantom: those files are simply never reached. Relative edges
//! are followed (with Node-style extension/index resolution); bare edges become
//! candidate dependencies.
//!
//! The walk is source-agnostic ([`FileSource`]): [`walk`] drives it over an
//! extracted directory tree (post-link), [`walk_index`] over a CAS-backed
//! `PackageIndex` (extract-time, before any navigable tree exists). Both share
//! the single [`walk_generic`] traversal, so an extract-time scan is guaranteed
//! to reach the same file set — and produce the same references — as a post-link
//! scan of the materialized tree.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{Entry, EntryKind};
use nub_phantom_core::extract::{Occurrence, extract, extract_optimized};
use nub_phantom_core::specifier::{self, SpecKind};

/// Extract one file's specifiers.
///
/// PRODUCTION DEFAULT is the BASELINE full-AST-visit `extract`, NOT the
/// static-imports-only `extract_optimized` ladder. This is the measured outcome
/// of the extraction-optimization investigation (the maintainer's ask): the
/// ladder was built, proven output-identical (see `extract_optimized`'s parity
/// test), and benchmarked inline over real trees — and it REGRESSES the scan
/// ~3-13% rather than helping. The scan cost is dominated by the oxc PARSE (which
/// builds the full AST to recover specifiers regardless — the deep-visit depth
/// the ladder skips is cheap next to it) and by fs I/O + the graph walk, not by
/// AST traversal; and reachable published npm code is CJS/`require`-heavy, so the
/// ladder's guard-aware full path is the COMMON case, not the exception it
/// assumed. So production stays on the simpler, marginally-faster baseline. The
/// `NUB_PHANTOM_EXTRACT_MODE=optimized` toggle re-enables the ladder for A/B
/// reproduction (scan-bench). Reading one env var per file is negligible next to
/// a parse; it is a pure measurement seam.
fn extract_file(rel: &str, source: &str) -> Vec<Occurrence> {
    if std::env::var_os("NUB_PHANTOM_EXTRACT_MODE").is_some_and(|v| v == "optimized") {
        extract_optimized(rel, source)
    } else {
        extract(rel, source)
    }
}

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

/// The two package-relative resolution + read operations the walk needs,
/// abstracted over its backing store. Implemented by [`FsSource`] (an extracted
/// tree) and [`IndexSource`] (a CAS-backed `PackageIndex`); the single generic
/// [`walk_generic`] keeps the two backings behaviorally identical.
trait FileSource {
    /// A resolved, canonical file identifier within the package.
    type Key: Ord + Clone;
    /// Resolve a published entry path (package-root-relative) to a file.
    fn resolve_entry(&self, entry_path: &str) -> Option<Self::Key>;
    /// Resolve a relative specifier as written inside `from`.
    fn resolve_rel(&self, from: &Self::Key, spec: &str) -> Option<Self::Key>;
    /// Read a file's UTF-8 content (None on read/decode failure).
    fn read(&self, key: &Self::Key) -> Option<String>;
    /// The package-relative path for `key` — the parser's `SourceType` hint.
    fn rel_path(&self, key: &Self::Key) -> String;
}

/// Walk from `entry_points`, following relative edges and collecting bare
/// references. Each reachable file accumulates a provenance mask (which entry
/// surface(s) reach it); a bare reference inherits its file's final mask, so the
/// report can separate subpath-adapter phantoms from main-graph ones.
///
/// Two phases keep provenance correct across diamonds: (1) BFS parses each file
/// once (cached) and propagates provenance bits to fixpoint — a file re-reached
/// with new bits is re-queued for propagation only, never re-parsed; (2) build
/// references from the cache using each file's FINAL mask.
fn walk_generic<S: FileSource>(source: &S, entry_points: &[Entry]) -> Walk {
    let mut result = Walk::default();
    let mut parsed: BTreeMap<S::Key, Vec<Occurrence>> = BTreeMap::new();
    let mut flags: BTreeMap<S::Key, u8> = BTreeMap::new();
    let mut queue: VecDeque<S::Key> = VecDeque::new();

    for ep in entry_points {
        if let Some(resolved) = source.resolve_entry(&ep.path) {
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
            let Some(text) = source.read(&file) else {
                continue;
            };
            let rel = source.rel_path(&file);
            // Extract via the BASELINE full-AST path by default (the optimized
            // ladder was measured to regress; `NUB_PHANTOM_EXTRACT_MODE=optimized`
            // re-enables it for A/B). See `extract_file`.
            parsed.insert(file.clone(), extract_file(&rel, &text));
        }
        // Collect relative edges first (immutable borrow of the cache) then
        // propagate — avoids holding a borrow across the flags mutation.
        let mut targets = Vec::new();
        for occ in &parsed[&file] {
            if let SpecKind::Relative = specifier::classify(&occ.spec) {
                match source.resolve_rel(&file, &occ.spec) {
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

/// Walk an already-extracted package tree rooted at `root`.
pub fn walk(root: &Path, entry_points: &[Entry]) -> Walk {
    walk_generic(&FsSource { root }, entry_points)
}

/// Walk a CAS-backed package: `files` are `(package-relative-path, absolute
/// CAS-blob-path)` pairs, as produced from a `PackageIndex` at extract time.
/// Resolution runs over the relative-path key set; content is read from the
/// paired blob. Output-identical to [`walk`] over the same package's extracted
/// tree (both drive [`walk_generic`]) for all real package layouts; see
/// [`normalize_rel_join`] for the one accepted divergence on malformed
/// (escape-and-re-enter-by-root-name) specifiers that never appear in practice.
pub fn walk_index(files: &[(String, PathBuf)], entry_points: &[Entry]) -> Walk {
    let map: BTreeMap<String, PathBuf> = files.iter().cloned().collect();
    walk_generic(&IndexSource { files: map }, entry_points)
}

/// OR `bit` into `key`'s provenance mask. Returns true if the mask GREW (new
/// file, or new bits) — the caller then (re)queues it so the new provenance
/// propagates to its edges. The 2-bit lattice bounds re-queues to ≤2 per file.
fn add_flags<K: Ord + Clone>(flags: &mut BTreeMap<K, u8>, key: &K, bit: u8) -> bool {
    let entry = flags.entry(key.clone()).or_insert(0);
    let before = *entry;
    *entry |= bit;
    *entry != before
}

/// Bound on `main`-chasing recursion. A dir whose `package.json` `main` points
/// back at itself (`"."`/`""`/`"./"`) or a mutual `main` cycle across dirs would
/// otherwise recurse forever → a stack-overflow ABORT that kills the whole scan
/// (a process abort, not a catchable panic). Such manifests occur in the wild, so
/// the cap is a hard robustness requirement, not a nicety.
const MAX_RESOLVE_DEPTH: u32 = 16;

/// Node-style resolution extensions, in priority order.
const RESOLVE_EXTS: [&str; 8] = ["js", "cjs", "mjs", "jsx", "ts", "tsx", "mts", "cts"];

// --- Filesystem-backed source (extracted tree) ---------------------------------

struct FsSource<'a> {
    root: &'a Path,
}

impl FileSource for FsSource<'_> {
    type Key = PathBuf;

    fn resolve_entry(&self, entry_path: &str) -> Option<PathBuf> {
        fs_resolve(self.root, self.root, entry_path, 0)
    }

    fn resolve_rel(&self, from: &PathBuf, spec: &str) -> Option<PathBuf> {
        let from_dir = from.parent().unwrap_or(self.root);
        fs_resolve(self.root, from_dir, spec, 0)
    }

    fn read(&self, key: &PathBuf) -> Option<String> {
        fs::read_to_string(key).ok()
    }

    fn rel_path(&self, key: &PathBuf) -> String {
        key.strip_prefix(self.root)
            .unwrap_or(key)
            .to_string_lossy()
            .into_owned()
    }
}

fn fs_resolve(root: &Path, from_dir: &Path, spec: &str, depth: u32) -> Option<PathBuf> {
    if depth > MAX_RESOLVE_DEPTH {
        return None;
    }
    let joined = from_dir.join(spec);
    // Keep the walk inside the package tree (a `../../` that climbs out is not
    // part of the published surface).
    let base = normalize(&joined);
    if !base.starts_with(root) {
        return None;
    }

    // 1. Exact file.
    if is_js_file(&base) {
        return Some(base);
    }
    // 2. `base.<ext>`.
    for ext in RESOLVE_EXTS {
        let cand = with_appended_ext(&base, ext);
        if is_js_file(&cand) {
            return Some(cand);
        }
    }
    // 3. `base/index.<ext>`.
    for ext in RESOLVE_EXTS {
        let cand = base.join(format!("index.{ext}"));
        if is_js_file(&cand) {
            return Some(cand);
        }
    }
    // 4. `base/package.json` → its `main` (depth-bounded; see MAX_RESOLVE_DEPTH).
    let pkg = base.join("package.json");
    if let Ok(raw) = fs::read(&pkg) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw)
            && let Some(main) = v.get("main").and_then(|m| m.as_str())
        {
            return fs_resolve(root, &base, main, depth + 1);
        }
        // package.json with no main → default index.js in that dir.
        for ext in RESOLVE_EXTS {
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

// --- Index-backed source (CAS blobs, no navigable tree) ------------------------

struct IndexSource {
    /// Package-relative path (POSIX `/`) → absolute CAS blob path.
    files: BTreeMap<String, PathBuf>,
}

impl FileSource for IndexSource {
    type Key = String;

    fn resolve_entry(&self, entry_path: &str) -> Option<String> {
        self.resolve("", entry_path, 0)
    }

    fn resolve_rel(&self, from: &String, spec: &str) -> Option<String> {
        self.resolve(parent_rel(from), spec, 0)
    }

    fn read(&self, key: &String) -> Option<String> {
        fs::read_to_string(self.files.get(key)?).ok()
    }

    fn rel_path(&self, key: &String) -> String {
        key.clone()
    }
}

impl IndexSource {
    /// True if `rel` is a JS-like file present in the index — the index analogue
    /// of `is_js_file` (name check + existence), which the fs ladder relies on.
    fn contains_js(&self, rel: &str) -> bool {
        crate::manifest::is_js_like(rel) && self.files.contains_key(rel)
    }

    /// Index analogue of [`fs_resolve`], step-for-step: exact → `base.<ext>` →
    /// `base/index.<ext>` → `base/package.json` `main`. Resolution is over the
    /// relpath key set; the `main` chase reads the package.json blob.
    fn resolve(&self, from_dir: &str, spec: &str, depth: u32) -> Option<String> {
        if depth > MAX_RESOLVE_DEPTH {
            return None;
        }
        // `None` == escaped the package root, mirroring fs `!starts_with(root)`.
        let base = normalize_rel_join(from_dir, spec)?;

        if self.contains_js(&base) {
            return Some(base);
        }
        for ext in RESOLVE_EXTS {
            let cand = format!("{base}.{ext}");
            if self.contains_js(&cand) {
                return Some(cand);
            }
        }
        for ext in RESOLVE_EXTS {
            let cand = join_rel(&base, &format!("index.{ext}"));
            if self.contains_js(&cand) {
                return Some(cand);
            }
        }
        let pkg = join_rel(&base, "package.json");
        if let Some(blob) = self.files.get(&pkg)
            && let Ok(raw) = fs::read(blob)
        {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw)
                && let Some(main) = v.get("main").and_then(|m| m.as_str())
            {
                return self.resolve(&base, main, depth + 1);
            }
            for ext in RESOLVE_EXTS {
                let cand = join_rel(&base, &format!("index.{ext}"));
                if self.contains_js(&cand) {
                    return Some(cand);
                }
            }
        }
        None
    }
}

/// The directory portion of a POSIX relpath (`a/b/c.js` → `a/b`, `c.js` → ``).
fn parent_rel(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Join a relpath dir and a leaf (`` + `x` → `x`; `a/b` + `x` → `a/b/x`).
fn join_rel(base: &str, leaf: &str) -> String {
    if base.is_empty() {
        leaf.to_string()
    } else {
        format!("{base}/{leaf}")
    }
}

/// Join `from_dir` + `spec` as POSIX relpaths and collapse `.`/`..`. Returns
/// `None` when a `..` climbs above the package root — the index analogue of the
/// fs `normalize` + `!starts_with(root)` reject. `from_dir` is the importing
/// file's directory (`""` at the root); `spec` may itself contain `/`.
fn normalize_rel_join(from_dir: &str, spec: &str) -> Option<String> {
    // An absolute specifier (`/foo`) replaces the base under fs `Path::join` and
    // then escapes the package root → fs resolves nothing. Reject it here rather
    // than dropping the leading `/` as an empty segment (which would follow an
    // edge fs never does). One fs corner is deliberately NOT matched: a spec that
    // climbs out and re-enters via the root's OWN basename (`../<rootname>/x`)
    // resolves under fs by absolute-path name coincidence but yields None here —
    // that input is malformed/never-published, and the stricter reject is the
    // safer reading, so this is an accepted divergence rather than a bug.
    if spec.starts_with('/') {
        return None;
    }
    let mut stack: Vec<&str> = from_dir.split('/').filter(|c| !c.is_empty()).collect();
    for comp in spec.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                // A pop with nothing to remove would climb above the root.
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{walk, walk_index};
    use crate::manifest::{Entry, EntryKind};
    use std::fs;
    use std::path::PathBuf;

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

    /// Build the `(relpath, blob-path)` pairs `walk_index` expects from an
    /// on-disk tree — here the blob IS the real file, so content is identical.
    fn index_of(root: &std::path::Path) -> Vec<(String, PathBuf)> {
        fn rec(base: &std::path::Path, cur: &std::path::Path, out: &mut Vec<(String, PathBuf)>) {
            for e in fs::read_dir(cur).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    rec(base, &p, out);
                } else {
                    let rel = p
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, p));
                }
            }
        }
        let mut out = Vec::new();
        rec(root, root, &mut out);
        out
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
    fn self_cyclic_main_does_not_stack_overflow() {
        // A dir whose package.json `main` points back at itself would recurse
        // forever without the depth cap → process abort, killing the whole scan.
        let root = scratch("cycle");
        fs::write(root.join("index.js"), "require('./lib');").unwrap();
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("lib/package.json"), r#"{"main":"."}"#).unwrap();
        // Must return (not abort); the cyclic dir simply resolves to nothing.
        let w = walk(&root, &[main_entry("index.js")]);
        assert_eq!(w.files_analyzed, 1); // only index.js; lib/ resolves to no file
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

    #[test]
    fn index_walk_matches_fs_walk_on_nested_resolution() {
        // A tree exercising every resolution rung: bare edge, `./x` → `x.js`,
        // a directory `./sub` → `sub/index.js`, and a `./pkg` dir resolved via
        // its `package.json` main. The index walk must reach the same files and
        // collect the same bare references as the fs walk.
        let root = scratch("index-parity");
        fs::write(
            root.join("index.js"),
            "require('./util'); require('./sub'); require('./pkg'); require('root-dep');",
        )
        .unwrap();
        fs::write(root.join("util.js"), "import 'util-dep';").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/index.js"), "require('sub-dep');").unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/package.json"), r#"{"main":"./entry.js"}"#).unwrap();
        fs::write(root.join("pkg/entry.js"), "import 'pkg-dep';").unwrap();

        let eps = [main_entry("index.js")];
        let fs_walk = walk(&root, &eps);
        let idx_walk = walk_index(&index_of(&root), &eps);

        assert_eq!(fs_walk.files_analyzed, idx_walk.files_analyzed);
        let mut a: Vec<_> = fs_walk
            .references
            .iter()
            .map(|r| r.package.clone())
            .collect();
        let mut b: Vec<_> = idx_walk
            .references
            .iter()
            .map(|r| r.package.clone())
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert!(a.iter().any(|p| p == "util-dep"));
        assert!(a.iter().any(|p| p == "sub-dep"));
        assert!(a.iter().any(|p| p == "pkg-dep"));
        assert!(a.iter().any(|p| p == "root-dep"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_walk_matches_fs_walk_on_absolute_specifier() {
        // An absolute specifier classifies as Relative and reaches the resolver;
        // fs `Path::join` makes it escape the root (→ None), and the index resolver
        // must reject it identically instead of treating the leading `/` as a
        // no-op. Parity: both walks reach only index.js and collect only `real`.
        let root = scratch("abs-spec");
        fs::write(
            root.join("index.js"),
            "require('/abs/thing'); require('real');",
        )
        .unwrap();
        let eps = [main_entry("index.js")];
        let fs_walk = walk(&root, &eps);
        let idx_walk = walk_index(&index_of(&root), &eps);
        assert_eq!(fs_walk.files_analyzed, 1);
        assert_eq!(fs_walk.files_analyzed, idx_walk.files_analyzed);
        let mut a: Vec<_> = fs_walk
            .references
            .iter()
            .map(|r| r.package.clone())
            .collect();
        let mut b: Vec<_> = idx_walk
            .references
            .iter()
            .map(|r| r.package.clone())
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(a, vec!["real".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }
}
