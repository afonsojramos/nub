//! Selective-subtree disk-materialization policy — nub's disk-materialize
//! expansion hook (the default; disabled by `NUB_DYNAMIC_PHANTOM_EJECT=0`).
//!
//! Disk-materializing a package project-local is only SOUND for a
//! transitively-consumed package if its whole ancestor-closure materializes with
//! it — otherwise a store-resident importer keeps resolving the un-materialized
//! shared-store copy, a silent singleton split (two realpaths, two module
//! instances). This hook expands aube's flat disk-materialize seed into a
//! graph-aware plan against the resolved lockfile:
//!
//! - **Rung 1 — ancestor-closure.** Each seed grows to
//!   [`LockfileGraph::importer_closure`] — the seed UNION every package that
//!   transitively imports it. Bounded to the affected subtree by construction
//!   (unrelated top-level subtrees are not importers), measured 0.3–2.1% of real
//!   large trees. Also SUBSUMES the #315 library-embedded-vite<8.1 residual: an
//!   embedded vite<8.1 (a framework's transitive engine, no direct-dep symlink)
//!   is auto-detected and its `[framework…vite]` closure ejected, so #318's dist
//!   sniff patch reaches a now-project-local vite.
//! - **Rung 2 — hoist-within.** For a transitively-phantom importer whose
//!   undeclared target the closure alone can't place, the already-resolved target
//!   is hoisted as an extra sibling within the importer's own materialized
//!   `node_modules`.
//!
//! The phantom importers + their undeclared targets are the DYNAMIC output of the
//! extract-time per-version scanner (`crate::dynamic_phantom`, the PRODUCER): it
//! scans each fetched version's real published code for unguarded undeclared
//! imports and writes a per-content verdict sidecar. This hook (the CONSUMER)
//! reads those sidecars — so there is no hand-maintained list of phantom classes;
//! the detection is per-version and auto-current. A precision SEED-SELECTION
//! filter (see [`should_seed`]) drops a flagged importer whose undeclared targets
//! are all already reachable inside its own subtree and absent from the project
//! top level, so a transitively-satisfied over-flag never ejects.
//!
//! Opt-out (`NUB_DYNAMIC_PHANTOM_EJECT=0`) ⇒ no hook installed ⇒ aube's
//! `expand_disk_materialize` returns the seed verbatim ⇒ the disk-materialize
//! pass is byte-for-byte the pre-productionization pure-symlink behavior. All
//! policy lives here; aube owns only the neutral seam + the graph primitive.

use std::collections::{BTreeMap, HashSet, VecDeque};

use aube_linker::DiskMaterializePlan;
use aube_lockfile::{LockedPackage, LockfileGraph};
use nub_phantom_scan::ScanResult;
use rayon::prelude::*;

/// The SINGLE phantom-eject arm — [`crate::dynamic_phantom::enabled`] — shared
/// with the extract-time producer so detection (the scanner), transitive
/// soundness (this closure), and warm-tree invalidation (the fingerprint) can
/// never disagree. On by DEFAULT; `NUB_DYNAMIC_PHANTOM_EJECT=0` is the opt-out.
/// The flag IS now folded into the install-state fingerprint (via the embedder
/// `extra_settings_fingerprint` hook; see [`crate::dynamic_phantom::settings_fingerprint`]),
/// so flipping it on an already-installed tree re-links to the new
/// materialization shape rather than accepting a stale node_modules.
fn enabled() -> bool {
    crate::dynamic_phantom::enabled()
}

/// Register nub's disk-materialize expansion hook with the embedded engine.
/// No-op only under the `NUB_DYNAMIC_PHANTOM_EJECT=0` opt-out, in which case
/// `aube_linker::expand_disk_materialize` stays the identity — byte-for-byte the
/// pure-symlink disk-materialize behavior. Set-once (idempotent); safe to call
/// once per engine-session build.
pub(crate) fn register() {
    if !enabled() {
        return;
    }
    aube_linker::set_disk_materialize_expand_hook(Box::new(expand));
}

/// The hook entry: read the per-version scanner's sidecars (the store-IO half)
/// then hand off to the pure planner. Split so [`plan_from_flags`] — all the
/// closure/seed policy — is unit-tested with injected flags and never touches
/// the host store.
fn expand(graph: &LockfileGraph, seed_names: &[String]) -> DiskMaterializePlan {
    plan_from_flags(graph, seed_names, &dynamic_phantom_flags(graph))
}

/// Pure planner: resolved graph + flat seed + dynamic phantom flags → graph-aware
/// materialization plan. See the module docs for the two rungs. `flags` is each
/// surviving-candidate importer's `(dep_path, name, undeclared-target-names)`,
/// supplied by [`dynamic_phantom_flags`] in production and injected directly in
/// tests.
fn plan_from_flags(
    graph: &LockfileGraph,
    seed_names: &[String],
    flags: &[(String, String, Vec<String>)],
) -> DiskMaterializePlan {
    // Top-level presence: default-hoist top level = the importer DIRECT deps. See
    // `should_seed` for why this gate is load-bearing and its non-default-hoist
    // scope caveat.
    let root_provided: HashSet<&str> = graph
        .importers
        .values()
        .flat_map(|deps| deps.iter().map(|d| d.name.as_str()))
        .collect();
    let is_top_level = |name: &str| root_provided.contains(name);

    // Seed set by NAME: the caller's disk-materialize list ∪ every dynamically-
    // flagged importer that SURVIVES the precision seed-selection filter. Embedded
    // vite<8.1 is seeded by dep_path below.
    let mut seed_names_set: HashSet<&str> = seed_names.iter().map(String::as_str).collect();

    // Dynamic phantom source (the per-version scanner's sidecars) — the replacement
    // for the retired hand-curated map. `dynamic_targets` keeps each SURVIVING
    // importer's undeclared targets keyed by dep_path, for the rung-2 hoist; only
    // survivors are kept, so a hoist only ever lands in a package the seed actually
    // materializes.
    let mut dynamic_targets: BTreeMap<&str, &[String]> = BTreeMap::new();
    for (dep_path, name, targets) in flags {
        if should_seed(targets, &reachable_dep_names(dep_path, graph), is_top_level) {
            seed_names_set.insert(name.as_str());
            dynamic_targets.insert(dep_path.as_str(), targets.as_slice());
        }
    }

    // Seed DEP_PATHS: every graph package whose name is a seed, plus every
    // embedded vite<8.1 copy (auto-detected — the #315 residual). Seeding by
    // dep_path keeps the reverse walk anchored to the real copies present.
    let mut seed_dep_paths: HashSet<&str> = HashSet::new();
    for (dep_path, pkg) in &graph.packages {
        if seed_names_set.contains(pkg.name.as_str())
            || (pkg.name == "vite" && super::vite_compat::vite_lt_8_1(&pkg.version))
        {
            seed_dep_paths.insert(dep_path.as_str());
        }
    }

    // Rung 1 — reverse-BFS ancestor-closure = the affected subtree.
    let closure = graph.importer_closure(seed_dep_paths.iter().copied());

    // Bounded-subtree guard: the closure must stay a small slice of the tree. A
    // closure approaching the whole tree means a foundational seed — a bug, since
    // phantom breakers are empirically never foundational. Surface it loudly;
    // never silently degrade to whole-tree materialization (that is `disableGVS`,
    // a separate last-resort lever). The `total >= 20` floor avoids a spurious
    // warning on a tiny fixture where a legitimate 2-3 package closure is
    // naturally a large fraction (e.g. a 3-package firebase repro).
    let total = graph.packages.len().max(1);
    if total >= 20 && closure.len() * 2 > total {
        tracing::warn!(
            "selective-subtree closure spans {}/{} packages ({:.0}%) — unexpectedly \
             large; a seed may be foundational (should not happen for a phantom breaker)",
            closure.len(),
            total,
            closure.len() as f64 / total as f64 * 100.0,
        );
    }

    // Rung-1 names: every closure member's name ∪ the original seed names (the
    // executor is name-keyed). Original seed names are kept even if absent from
    // the graph — the executor simply never matches an absent name.
    let mut names: HashSet<String> = seed_names.iter().cloned().collect();
    for dep_path in &closure {
        if let Some(pkg) = graph.packages.get(dep_path) {
            names.insert(pkg.name.clone());
        }
    }

    // Rung-2 hoist map: for each closure member that is a surviving dynamically-
    // flagged importer, resolve its undeclared target(s) to a dep_path present in
    // the graph and record importer_dep_path → [target_dep_paths]. A survivor is
    // seeded by name, so its dep_path is in the closure by construction; iterating
    // the closure keeps this aligned to what actually materializes.
    let mut hoist_within: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dep_path in &closure {
        let Some(targets) = dynamic_targets.get(dep_path.as_str()) else {
            continue;
        };
        let resolved: Vec<String> = targets
            .iter()
            .filter_map(|t| resolve_target_dep_path(graph, t))
            .collect();
        if !resolved.is_empty() {
            hoist_within.insert(dep_path.clone(), resolved);
        }
    }

    DiskMaterializePlan {
        names: names.into_iter().collect(),
        hoist_within,
    }
}

/// The dynamic analogue of the retired hand-curated phantom-class map: for each
/// resolved package whose per-content sidecar (written by the extract-time
/// PRODUCER, [`crate::dynamic_phantom`]) reports an unguarded phantom, its
/// `(dep_path, name, undeclared-target-names)`.
///
/// Reads the SAME store handle + sidecar dir the producer wrote, via the shared
/// [`crate::dynamic_phantom`] path helpers, so the two cannot drift. Best-effort:
/// a package absent from the default store (a `store-dir` override moves the CAS),
/// a missing/torn sidecar, or a not-yet-scanned version all degrade to "not
/// flagged" — a scan miss never itself forces materialization. Fans out across
/// rayon; the backfill already warmed the sidecars, so each item is a small
/// cached-JSON index load + a blake3 fingerprint + a small sidecar read.
///
/// Empty under the `NUB_DYNAMIC_PHANTOM_EJECT=0` opt-out. In production `expand`
/// installs the hook only when armed, so this gate is belt-and-suspenders; the
/// pure planning logic is tested through [`plan_from_flags`] with injected flags,
/// so the unit tests never reach this store-IO path.
fn dynamic_phantom_flags(graph: &LockfileGraph) -> Vec<(String, String, Vec<String>)> {
    if !enabled() {
        return Vec::new();
    }
    let (Some(store_v1), Some(sidecar_dir)) = (
        crate::dynamic_phantom::store_v1_dir(),
        crate::dynamic_phantom::phantom_cache_dir(),
    ) else {
        return Vec::new();
    };
    // `Store::at` takes the CAS `files/` root; `store_v1` is its parent — the same
    // derivation the producer's backfill uses, so both key the index identically.
    let store = aube_store::Store::at(store_v1.join("files"));
    // BTreeMap has no rayon bridge; collect the resolved set first (as the
    // backfill does).
    let packages: Vec<(&String, &LockedPackage)> = graph.packages.iter().collect();
    packages
        .into_par_iter()
        .filter_map(|(dep_path, pkg)| {
            // `registry_name()` + `integrity` key the index the SAME way the
            // producer's backfill does, so npm-alias deps resolve to the right blob.
            let index =
                store.load_index(pkg.registry_name(), &pkg.version, pkg.integrity.as_deref())?;
            let fingerprint = aube_store::index_content_fingerprint(&index);
            // Derive the sidecar path through the SAME helper the producer writes
            // with, so the fingerprint keying and the scanner-version segment
            // cannot drift between the two halves.
            let bytes = std::fs::read(crate::dynamic_phantom::sidecar_path(
                &sidecar_dir,
                &fingerprint,
            ))
            .ok()?;
            // nub-cli CAN depend on the scanner crate, so the sidecar deserializes
            // straight into the typed `ScanResult` (no cross-fork string coupling).
            let result: ScanResult = serde_json::from_slice(&bytes).ok()?;
            if !result.has_unguarded_phantom {
                return None;
            }
            let targets: Vec<String> = result.targets.into_iter().map(|t| t.name).collect();
            Some((dep_path.clone(), pkg.name.clone(), targets))
        })
        .collect()
}

/// Whether a dynamically-flagged package must SEED the closure — the precision
/// filter, applied as seed-selection. DEFAULT is SEED (eject); a flag is
/// downgraded to a SKIP (not seeded) only when it can PROVE every undeclared
/// target is BOTH reachable inside the package's own resolved subtree AND absent
/// from the project top level.
///
/// SAFETY INVARIANT (non-negotiable): a wrong SKIP is a real phantom BREAK,
/// strictly worse than a redundant over-eject. So every uncertainty — a
/// target-less flag, a target outside the closure, a top-level target — falls
/// through to SEED. The filter only ever REMOVES an over-seed it can prove safe.
///
/// Why the top-level gate is load-bearing: under GVS there is no hidden hoist
/// tree, so an ejected (project-local) realpath additionally reaches the PROJECT
/// top level in its `node_modules` walk, while a skipped (shared-store) realpath
/// reaches only its own siblings. The eject therefore changes resolution for
/// exactly one class of target — those present at the project top level: a
/// top-level target resolves only when ejected (skipping it 404s), while a
/// non-top-level target is unresolvable in either state (skipping it is a true
/// no-op). (Corpus: es-abstract / typed-array-byte-length are transitively
/// satisfied → SKIP; @hookform/resolvers / swiper / @firebase/database are real
/// breakers → SEED.)
///
/// SCOPE CAVEAT (default-off flag): `is_top_level` here sees only the importer
/// DIRECT deps (`graph.importers`) — exactly the DEFAULT hoist config's top level.
/// The expand seam has no access to the linker's `public-hoist` / `shamefully-
/// hoist` config, so under a NON-default hoist config a target hoisted there (but
/// not a direct importer dep) is invisible to this check and could permit a skip
/// the linker-side gate would have ejected. Acceptable under the experimental flag
/// (the validated corpus is default-hoist); threading the hoist config into the
/// seam is the productionization fix.
fn should_seed(
    targets: &[String],
    reachable: &HashSet<String>,
    is_top_level: impl Fn(&str) -> bool,
) -> bool {
    if targets.is_empty() {
        return true;
    }
    !targets
        .iter()
        .all(|t| reachable.contains(t) && !is_top_level(t))
}

/// The set of package NAMES transitively reachable from the package at
/// `root_dep_path` through its resolved `dependencies` edges — the siblings a
/// phantom import from that package can satisfy WITHIN its own subtree, with no
/// project-local disk copy.
///
/// Walks `dependencies` ONLY, deliberately. Per [`LockedPackage`]'s contract that
/// map is the resolved edge set, with ACTIVE optional edges and RESOLVED peer
/// versions already MIRRORED into it — exactly the siblings symlinked into each
/// node's `node_modules`. Also walking the raw `optional_dependencies` /
/// `peer_dependencies` maps could only ADD edges ABSENT from `dependencies`
/// (platform-pruned optionals, or unresolved peer RANGES that never key
/// `graph.packages`); counting either as reachable would OVER-complete the
/// closure — the one direction that yields a wrong SKIP. So `dependencies` is both
/// the complete AND the safe edge source; this mirrors
/// [`LockfileGraph::importer_closure`]'s reconstruction of child keys.
///
/// A child NAME enters only when its full dep_path (`{name}@{tail}`, the tail
/// carrying any peer suffix) resolves in `graph.packages`; an unresolvable edge
/// halts that branch, leaving the closure a SUBSET of true reachability — which
/// errs toward SEED, never toward a wrong skip. A visited set + a hard node cap
/// bound a cyclic/adversarial graph; hitting the cap truncates, again toward SEED.
fn reachable_dep_names(root_dep_path: &str, graph: &LockfileGraph) -> HashSet<String> {
    // Ceiling far above any real subtree (hundreds of nodes); it only bounds a
    // pathological graph, and truncation errs toward seed.
    const MAX_NODES: usize = 100_000;
    let mut names = HashSet::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(root_dep_path.to_string());
    queue.push_back(root_dep_path.to_string());
    while let Some(dep_path) = queue.pop_front() {
        if visited.len() >= MAX_NODES {
            break;
        }
        let Some(pkg) = graph.packages.get(&dep_path) else {
            continue;
        };
        for (child_name, child_tail) in &pkg.dependencies {
            let child_key = format!("{child_name}@{child_tail}");
            if !graph.packages.contains_key(&child_key) {
                continue;
            }
            names.insert(child_name.clone());
            if visited.insert(child_key.clone()) {
                queue.push_back(child_key);
            }
        }
    }
    names
}

/// Resolve a target package NAME to a dep_path present in the graph. `packages`
/// is a `BTreeMap`, so `.find` yields the lexically-first matching dep_path —
/// deterministic, and unambiguous for the single-version phantom classes. The
/// scanner reports targets by name; per-version target dep_paths would need the
/// scanner to also record the resolved coordinate.
fn resolve_target_dep_path(graph: &LockfileGraph, target_name: &str) -> Option<String> {
    graph
        .packages
        .iter()
        .find(|(_, pkg)| pkg.name == target_name)
        .map(|(dep_path, _)| dep_path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test-graph edge: `(dep_path, name, [(child_name, child_tail)])`.
    type Edge<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

    fn graph(edges: &[Edge]) -> LockfileGraph {
        let mut g = LockfileGraph::default();
        for (dep_path, name, deps) in edges {
            let mut pkg = LockedPackage {
                name: name.to_string(),
                dep_path: dep_path.to_string(),
                ..Default::default()
            };
            // A real graph carries `version`; the vite<8.1 seed test needs it, so
            // parse it off the dep_path tail.
            if let Some((_, tail)) = split(dep_path) {
                pkg.version = tail.split('(').next().unwrap_or(tail).to_string();
            }
            for (cn, ct) in *deps {
                pkg.dependencies.insert(cn.to_string(), ct.to_string());
            }
            g.packages.insert(dep_path.to_string(), pkg);
        }
        g
    }

    fn split(dep_path: &str) -> Option<(&str, &str)> {
        let core_end = dep_path.find('(').unwrap_or(dep_path.len());
        let at = dep_path[..core_end].rfind('@')?;
        if at == 0 {
            return None;
        }
        Some((&dep_path[..at], &dep_path[at + 1..]))
    }

    fn names(xs: &[&str]) -> HashSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // Rung-1 vite seeding is independent of the dynamic source, so these inject
    // an EMPTY flag set and exercise the pure planner (`plan_from_flags`) end to
    // end — no host-store IO.

    #[test]
    fn embedded_vite_lt_8_1_seeds_its_framework_closure() {
        // astro → vite@6.4.3 (embedded, <8.1). The closure disk-materializes
        // BOTH so the ejected vite is project-local for the #318 patch.
        let g = graph(&[
            ("astro@5.0.0", "astro", &[("vite", "6.4.3")]),
            ("vite@6.4.3", "vite", &[]),
            // an unrelated modern vite direct dep must NOT drag anything in
            ("lodash@4.17.21", "lodash", &[]),
        ]);
        let plan = plan_from_flags(&g, &[], &[]);
        let plan_names: HashSet<&str> = plan.names.iter().map(String::as_str).collect();
        assert!(plan_names.contains("vite"), "embedded vite<8.1 seeded");
        assert!(plan_names.contains("astro"), "framework in the closure");
        assert!(
            !plan_names.contains("lodash"),
            "unrelated dep stays symlinked"
        );
    }

    #[test]
    fn modern_vite_alone_is_not_seeded() {
        let g = graph(&[
            ("app@1.0.0", "app", &[("vite", "8.1.3")]),
            ("vite@8.1.3", "vite", &[]),
        ]);
        let plan = plan_from_flags(&g, &[], &[]);
        assert!(
            plan.names.is_empty(),
            "vite>=8.1 needs no eject (Unit A covers it)"
        );
    }

    #[test]
    fn no_seeds_yields_empty_plan() {
        let g = graph(&[("lodash@4.17.21", "lodash", &[])]);
        let plan = plan_from_flags(&g, &[], &[]);
        assert!(plan.names.is_empty() && plan.hoist_within.is_empty());
    }

    #[test]
    fn dynamic_flag_seeds_importer_and_records_hoist() {
        // The now-default path: a phantom adapter (`@hookform/resolvers`)
        // statically imports an undeclared `zod`. The target isn't reachable
        // within the adapter's own subtree, so `should_seed` SEEDS it; rung-2
        // resolves `zod` to its dep_path and records the hoist into the adapter's
        // materialized `node_modules`.
        let g = graph(&[
            ("@hookform/resolvers@1.0.0", "@hookform/resolvers", &[]),
            ("zod@3.0.0", "zod", &[]),
        ]);
        let flags = vec![(
            "@hookform/resolvers@1.0.0".to_string(),
            "@hookform/resolvers".to_string(),
            vec!["zod".to_string()],
        )];
        let plan = plan_from_flags(&g, &[], &flags);
        let names: HashSet<&str> = plan.names.iter().map(String::as_str).collect();
        assert!(
            names.contains("@hookform/resolvers"),
            "the flagged phantom importer is seeded"
        );
        assert_eq!(
            plan.hoist_within
                .get("@hookform/resolvers@1.0.0")
                .map(Vec::as_slice),
            Some(["zod@3.0.0".to_string()].as_slice()),
            "rung-2 records the undeclared target's hoist"
        );
    }

    // Precision seed-selection (`should_seed`) — the port of the retired link-time
    // filter, tested as a pure function.

    #[test]
    fn seed_unless_every_target_in_closure_and_non_top_level() {
        let closure = names(&["a", "b", "for-each"]);
        let none_top_level = |_: &str| false;
        // All targets in-closure, none at the project top level → safe SKIP.
        assert!(!should_seed(
            &["a".to_string(), "b".to_string()],
            &closure,
            none_top_level
        ));
        // A target outside the closure → SEED (can't prove safe).
        assert!(should_seed(
            &["a".to_string(), "missing".to_string()],
            &closure,
            none_top_level
        ));
        // A target-less flag → SEED (can't prove safe on incomplete info).
        assert!(should_seed(&[], &closure, none_top_level));
    }

    #[test]
    fn top_level_target_forces_seed_even_when_in_closure() {
        // The wrong-skip hole: `for-each` is in-closure BUT also at the project
        // top level, so an ejected (project-local) copy resolves it while a
        // skipped (shared-store) one 404s — the eject is load-bearing, so the
        // top-level gate must veto the skip.
        let closure = names(&["for-each", "a"]);
        let for_each_top_level = |n: &str| n == "for-each";
        assert!(should_seed(
            &["for-each".to_string()],
            &closure,
            for_each_top_level
        ));
        // A different, non-top-level in-closure target stays safe to skip.
        assert!(!should_seed(
            &["a".to_string()],
            &closure,
            for_each_top_level
        ));
    }

    // Reachability (`reachable_dep_names`) — the in-subtree name set the precision
    // filter consults.

    #[test]
    fn reachable_names_reconstruct_from_tails_including_peer_suffix() {
        // P → a → shared; P → styled(peer suffix in the tail). The full graph key
        // for the peer-context child carries the `(react@18.2.0)` suffix — the
        // reconstruction must reproduce it exactly.
        let g = graph(&[
            (
                "p@1.0.0",
                "p",
                &[("a", "1.0.0"), ("styled", "6.0.0(react@18.2.0)")],
            ),
            ("a@1.0.0", "a", &[("shared", "2.0.0")]),
            ("shared@2.0.0", "shared", &[]),
            ("styled@6.0.0(react@18.2.0)", "styled", &[]),
        ]);
        let r = reachable_dep_names("p@1.0.0", &g);
        assert!(r.contains("a"), "direct dep");
        assert!(r.contains("shared"), "transitive dep");
        assert!(r.contains("styled"), "peer-suffixed tail reconstructs");
        assert!(!r.contains("p"), "root itself is not in its own closure");
    }

    #[test]
    fn reachable_names_halt_on_unresolvable_edge_toward_seed() {
        // P declares `a@9.9.9`, absent from the graph → the edge does not resolve,
        // so `a` and its subtree (`deep`) stay out of the closure → a phantom
        // target of either SEEDS (the safe direction).
        let g = graph(&[
            ("p@1.0.0", "p", &[("a", "9.9.9")]),
            ("a@1.0.0", "a", &[("deep", "1.0.0")]),
            ("deep@1.0.0", "deep", &[]),
        ]);
        let r = reachable_dep_names("p@1.0.0", &g);
        assert!(!r.contains("a"), "unresolved edge is not counted reachable");
        assert!(!r.contains("deep"), "subtree behind it is unreached");
    }

    #[test]
    fn reachable_names_terminate_on_cycles() {
        // A ↔ B mutual dependency must not stall the bounded walk.
        let g = graph(&[
            ("p@1.0.0", "p", &[("a", "1.0.0")]),
            ("a@1.0.0", "a", &[("b", "1.0.0")]),
            ("b@1.0.0", "b", &[("a", "1.0.0")]),
        ]);
        let r = reachable_dep_names("p@1.0.0", &g);
        assert!(r.contains("a") && r.contains("b"));
    }
}
