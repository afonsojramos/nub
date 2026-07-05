//! nub-phantom-scan — scan an already-extracted package version's PUBLISHED,
//! reachable code for UNDECLARED (phantom) dependencies, and reduce it to the
//! target-agnostic boolean the disk-eject decision needs.
//!
//! This is the shared home of the reachable-graph walk + verdict layer, lifted
//! out of the excluded `nub-phantom` eval tool so the shipped `nub` CLI's dynamic
//! per-version scan-on-link drives ejection from the SAME pipeline. The pipeline:
//!
//!   walk the module graph from `exports`/`main`/`bin` → extract import/require
//!   specifiers (via `nub-phantom-core`'s oxc parser) → classify each against the
//!   declared surface.
//!
//! For the dynamic detector the actionable output is [`ScanResult`]: a single
//! `has_unguarded_phantom` boolean (does this version statically, unguardedly
//! import a package it does not declare?) plus the offending target set with
//! provenance. Disk-eject is target-agnostic, so the boolean is all the eject
//! DECISION consults; the targets carry the provenance a later transitive
//! (subtree) reachability query would use.

pub mod classify;
pub mod graph;
pub mod manifest;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use classify::Finding;
pub use classify::Verdict;
use manifest::Manifest;

/// One undeclared target a scanned version pulls in, with the provenance bits the
/// eject reachability query needs. Serialized into the per-integrity sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhantomTarget {
    /// The undeclared package name (`zod`, `@apify/datastructures`).
    pub name: String,
    /// Reached from the package's main entry surface.
    pub from_main: bool,
    /// Reached from a non-`.` `exports` subpath (the adapter surface).
    pub from_subpath: bool,
}

/// The per-version scan verdict — the immutable, per-content-integrity payload
/// the dynamic detector caches and feeds to disk-eject.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanResult {
    /// The eject DECISION: at least one HARD (unguarded) undeclared import in the
    /// reachable published graph. Disk-eject is target-agnostic, so this boolean
    /// is what the eject decision consults.
    pub has_unguarded_phantom: bool,
    /// The offending undeclared targets (empty when `has_unguarded_phantom` is
    /// false). Provenance-tagged for the later transitive/subtree reachability
    /// query; the per-package eject decision itself needs only the boolean.
    pub targets: Vec<PhantomTarget>,
    /// Reachable files parsed (diagnostic — lets the caller see scan breadth).
    pub files_analyzed: usize,
}

/// Scan an already-extracted package tree rooted at `root` (the dir holding
/// `package.json`) and reduce to a [`ScanResult`]. A tree that can't be parsed
/// (no/!readable `package.json`) yields `None` — the caller treats it as
/// "nothing to eject" (a scan miss must never itself force materialization).
pub fn scan_extracted(root: &Path) -> Option<ScanResult> {
    let raw = std::fs::read(root.join("package.json")).ok()?;
    let manifest = Manifest::parse(&raw)?;
    let walk = graph::walk(root, &manifest.entry_points);
    Some(reduce(&manifest, &walk))
}

/// Scan a package straight from its CAS-backed file index — the EXTRACT-TIME
/// entry, run inside the store's tarball import before any navigable tree
/// exists. `files` are `(package-relative-path, absolute CAS-blob-path)` pairs
/// projected from a `PackageIndex`: resolution runs over the relpath key set and
/// content is read from the paired blob. Returns the SAME [`ScanResult`] as
/// [`scan_extracted`] over the equivalent extracted tree (both reduce a shared
/// [`graph`] walk). `None` when the package has no parseable `package.json`.
pub fn scan_index(files: &[(String, PathBuf)]) -> Option<ScanResult> {
    let pkg_json = files.iter().find(|(rel, _)| rel == "package.json")?;
    let raw = std::fs::read(&pkg_json.1).ok()?;
    let manifest = Manifest::parse(&raw)?;
    let walk = graph::walk_index(files, &manifest.entry_points);
    Some(reduce(&manifest, &walk))
}

/// Reduce a completed reachable-graph walk to the eject [`ScanResult`]: classify
/// each reference, keep the HARD (unguarded) undeclared ones as targets, and set
/// the boolean. The single reduction shared by both scan entries — output
/// identity of `scan_index` and `scan_extracted` rests on this.
fn reduce(manifest: &Manifest, walk: &graph::Walk) -> ScanResult {
    let findings = classify::classify(manifest, &walk.references);
    let targets: Vec<PhantomTarget> = findings
        .iter()
        .filter(|f| f.verdict == Verdict::HardPhantom)
        .map(|f| PhantomTarget {
            name: f.package.clone(),
            from_main: f.from_main,
            from_subpath: f.from_subpath,
        })
        .collect();
    ScanResult {
        has_unguarded_phantom: !targets.is_empty(),
        targets,
        files_analyzed: walk.files_analyzed,
    }
}

/// The full per-package report (all verdict categories, not just hard phantoms) —
/// used by the eval tool and the A/B corpus harness. `scan_extracted` is the lean
/// production entry; this is the diagnostic one.
#[derive(Debug, Serialize)]
pub struct PackageReport {
    pub name: String,
    pub version: String,
    pub findings: Vec<Finding>,
    pub files_analyzed: usize,
    pub unresolved_relative: usize,
}

impl PackageReport {
    pub fn count(&self, v: Verdict) -> usize {
        self.findings.iter().filter(|f| f.verdict == v).count()
    }
    pub fn hard_phantoms(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.verdict == Verdict::HardPhantom)
    }
    pub fn subpath_adapter_phantoms(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.is_subpath_adapter())
    }
    pub fn naive_phantom_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| {
                matches!(
                    f.verdict,
                    Verdict::HardPhantom | Verdict::SoftPhantom | Verdict::DeclaredOptionalPeer
                )
            })
            .count()
    }
}

/// Analyze an already-extracted package tree into a full [`PackageReport`].
pub fn analyze_extracted(root: &Path, version: &str) -> Result<PackageReport, String> {
    let raw =
        std::fs::read(root.join("package.json")).map_err(|e| format!("read package.json: {e}"))?;
    let manifest = Manifest::parse(&raw).ok_or("unparseable package.json / no name")?;
    let walk = graph::walk(root, &manifest.entry_points);
    let findings = classify::classify(&manifest, &walk.references);
    Ok(PackageReport {
        name: manifest.name,
        version: version.to_string(),
        findings,
        files_analyzed: walk.files_analyzed,
        unresolved_relative: walk.unresolved_relative,
    })
}

#[cfg(test)]
mod tests {
    use super::{PathBuf, Verdict, analyze_extracted, scan_extracted, scan_index};
    use std::fs;

    /// `(relpath, blob-path)` pairs for an on-disk tree — the blob is the real
    /// file, so `scan_index` reads byte-identical content to `scan_extracted`.
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

    fn fixture() -> std::path::PathBuf {
        // Per-call unique dir: several tests build a fixture concurrently under
        // cargo's parallel runner, so a shared `process::id()`-only path would
        // let one test's cleanup wipe another's tree mid-scan.
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "nub-phantom-scan-e2e-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{
                "name": "demo",
                "main": "index.js",
                "exports": { ".": "./index.js", "./adapter": "./adapter.js" },
                "dependencies": { "declared-dep": "1" },
                "peerDependencies": { "zod": "*" },
                "peerDependenciesMeta": { "zod": { "optional": true } }
            }"#,
        )
        .unwrap();
        fs::write(
            root.join("index.js"),
            r#"const a = require('declared-dep');
               const ghost = require('undeclared-ghost');
               let opt; try { opt = require('soft-ghost'); } catch {}
               require('./reached');"#,
        )
        .unwrap();
        fs::write(root.join("reached.js"), "import x from 'reached-ghost';").unwrap();
        fs::write(root.join("adapter.js"), "import 'backend-lib';").unwrap();
        root
    }

    #[test]
    fn scan_result_flags_hard_phantoms_only() {
        let root = fixture();
        let r = scan_extracted(&root).unwrap();
        assert!(r.has_unguarded_phantom);
        let names: Vec<&str> = r.targets.iter().map(|t| t.name.as_str()).collect();
        // Hard phantoms: undeclared-ghost, reached-ghost, backend-lib. NOT the
        // optional peer (zod), NOT the soft/try-guarded (soft-ghost).
        assert!(names.contains(&"undeclared-ghost"));
        assert!(names.contains(&"reached-ghost"));
        assert!(names.contains(&"backend-lib"));
        assert!(!names.contains(&"soft-ghost"));
        assert!(!names.contains(&"zod"));
        // Provenance: backend-lib is subpath-only (the adapter class).
        let backend = r.targets.iter().find(|t| t.name == "backend-lib").unwrap();
        assert!(backend.from_subpath && !backend.from_main);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_package_has_no_phantom() {
        let root =
            std::env::temp_dir().join(format!("nub-phantom-scan-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"clean","main":"index.js","dependencies":{"lodash":"1"}}"#,
        )
        .unwrap();
        fs::write(
            root.join("index.js"),
            "const _ = require('lodash'); const fs = require('node:fs');",
        )
        .unwrap();
        let r = scan_extracted(&root).unwrap();
        assert!(!r.has_unguarded_phantom);
        assert!(r.targets.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_index_is_output_identical_to_scan_extracted() {
        // The hard requirement: an extract-time index scan yields the exact same
        // verdict + target set + file count as a post-link tree scan. Uses the
        // full phantom fixture (main graph + `./adapter` subpath, hard/soft/peer
        // mix) so provenance and classification are both exercised.
        let root = fixture();
        let from_tree = scan_extracted(&root).unwrap();
        let from_index = scan_index(&index_of(&root)).unwrap();
        assert_eq!(
            from_index, from_tree,
            "extract-time index scan diverged from post-link tree scan"
        );
        assert!(from_index.has_unguarded_phantom);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn full_report_still_available() {
        let root = fixture();
        let r = analyze_extracted(&root, "1.2.3").unwrap();
        assert_eq!(r.count(Verdict::HardPhantom), 3);
        assert_eq!(r.version, "1.2.3");
        let _ = fs::remove_dir_all(&root);
    }
}
