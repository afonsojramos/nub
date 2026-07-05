//! Selective-subtree force-materialization policy — nub's force-materialize
//! expansion hook (behind the default-off `NUB_DYNAMIC_PHANTOM_EJECT` flag).
//!
//! Force-materializing a package project-local is only SOUND for a
//! transitively-consumed package if its whole ancestor-closure materializes with
//! it — otherwise a store-resident importer keeps resolving the un-materialized
//! shared-store copy, a silent singleton split (two realpaths, two module
//! instances). This hook expands aube's flat force-materialize seed into a
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
//!   undeclared target the closure alone can't place (`@nuxt/devtools`→
//!   `unstorage`, `vue-router`→`@vue/compiler-sfc`, `@firebase/database`→
//!   `@firebase/app`), the already-resolved target is hoisted as an extra
//!   sibling within the importer's own materialized `node_modules`.
//!
//! Flag OFF ⇒ no hook installed ⇒ aube's `expand_force_materialize` returns the
//! seed verbatim ⇒ the force-materialize pass is byte-for-byte the shipped
//! behavior. All policy lives here; aube owns only the neutral seam + the graph
//! primitive.

use std::collections::{BTreeMap, HashSet};

use aube_linker::ForceMaterializePlan;
use aube_lockfile::LockfileGraph;

/// Whether selective-subtree force-materialization is armed. Truthy
/// `NUB_DYNAMIC_PHANTOM_EJECT` (`1`/`true`/`yes`, or any non-falsey value) arms
/// it; unset or a falsey value (`0`/`false`/`no`/`off`/empty) is off — the
/// default. Same flag the (unmerged) dynamic per-version scanner will gate on, so
/// the closure (transitive soundness) and the scanner (detection) share one arm.
///
/// LIMITATION (experimental-flag scope): the flag is NOT folded into the
/// install-state fingerprint, which hashes only the raw `forceMaterializePackages`
/// setting + the lockfile. So flipping this flag on an ALREADY-installed tree
/// (unchanged lockfile) is a no-op — the link phase is skipped as "up to date". A
/// clean install (or one whose lockfile changed) picks it up. Acceptable for a
/// default-off experimental flag; folding the flag into the fingerprint is the
/// productionization fix (an aube `state.rs` change, kept default-preserving).
fn enabled() -> bool {
    match std::env::var("NUB_DYNAMIC_PHANTOM_EJECT") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// Known transitive-phantom classes: an importer that statically imports an
/// undeclared (or optional-peer-but-unlinked) target ALREADY present in the tree.
/// Each entry both SEEDS its importer for force-materialization and drives the
/// rung-2 hoist of the target within it.
///
/// PROVISIONAL: the dynamic per-version phantom scanner (the extract-time scan
/// this same flag gates, unmerged on `scanner-build`) is the intended auto-source
/// that supersedes this curated map — it scans each version's real code for
/// undeclared imports, so it needs no hand-maintenance. This small map exists so
/// the known acceptance classes (Nuxt, Firebase) work end-to-end today; it is the
/// rung-2 analogue of the shipped static `NUB_FORCE_MATERIALIZE_PACKAGES` list.
const KNOWN_PHANTOM_TARGETS: &[(&str, &[&str])] = &[
    // Nuxt 4: vue-router statically imports @vue/compiler-sfc, declared only as
    // an optional peer, so it is not auto-linked under the isolated GVS layout.
    ("vue-router", &["@vue/compiler-sfc"]),
    // Nuxt 4: @nuxt/devtools (stable 3.x) imports unstorage without declaring it
    // (nitropack, a sibling, is the package that declares it).
    ("@nuxt/devtools", &["unstorage"]),
    // Firebase: @firebase/database imports the @firebase/app singleton it does
    // not declare (its siblings @firebase/firestore / @firebase/auth DO declare
    // it as a peer — a per-package omission, not a design choice).
    ("@firebase/database", &["@firebase/app"]),
];

/// Register nub's force-materialize expansion hook with the embedded engine.
/// No-op unless `NUB_DYNAMIC_PHANTOM_EJECT` is armed, so the default path installs
/// nothing and `aube_linker::expand_force_materialize` stays the identity —
/// byte-for-byte the shipped force-materialize behavior. Set-once (idempotent);
/// safe to call once per engine-session build.
pub(crate) fn register() {
    if !enabled() {
        return;
    }
    aube_linker::set_force_materialize_expand_hook(Box::new(expand));
}

/// The hook body: resolved graph + flat seed → graph-aware materialization plan.
/// See the module docs for the two rungs.
fn expand(graph: &LockfileGraph, seed_names: &[String]) -> ForceMaterializePlan {
    // Seed set by NAME: the caller's force-materialize list ∪ the known
    // phantom-importer names. Embedded vite<8.1 is seeded by dep_path below.
    let mut seed_names_set: HashSet<&str> = seed_names.iter().map(String::as_str).collect();
    for (importer, _) in KNOWN_PHANTOM_TARGETS {
        seed_names_set.insert(importer);
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

    // Rung-2 hoist map: for each known phantom-importer whose dep_path landed in
    // the closure, resolve its target(s) to a dep_path present in the graph and
    // record importer_dep_path → [target_dep_paths].
    let mut hoist_within: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dep_path in &closure {
        let Some(pkg) = graph.packages.get(dep_path) else {
            continue;
        };
        let Some((_, targets)) = KNOWN_PHANTOM_TARGETS
            .iter()
            .find(|(name, _)| *name == pkg.name)
        else {
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

    ForceMaterializePlan {
        names: names.into_iter().collect(),
        hoist_within,
    }
}

/// Resolve a target package NAME to a dep_path present in the graph. `packages`
/// is a `BTreeMap`, so `.find` yields the lexically-first matching dep_path —
/// deterministic, and unambiguous for the single-version acceptance classes. The
/// dynamic scanner supplies precise per-version targeting when it lands.
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
    use aube_lockfile::LockedPackage;

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
            // A real graph carries `version`; several packages need it for the
            // vite<8.1 seed test, so parse it off the dep_path tail.
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

    #[test]
    fn embedded_vite_lt_8_1_seeds_its_framework_closure() {
        // astro → vite@6.4.3 (embedded, <8.1). The closure force-materializes
        // BOTH so the ejected vite is project-local for the #318 patch.
        let g = graph(&[
            ("astro@5.0.0", "astro", &[("vite", "6.4.3")]),
            ("vite@6.4.3", "vite", &[]),
            // an unrelated modern vite direct dep must NOT drag anything in
            ("lodash@4.17.21", "lodash", &[]),
        ]);
        let plan = expand(&g, &[]);
        let names: HashSet<&str> = plan.names.iter().map(String::as_str).collect();
        assert!(names.contains("vite"), "embedded vite<8.1 seeded");
        assert!(names.contains("astro"), "framework in the closure");
        assert!(!names.contains("lodash"), "unrelated dep stays symlinked");
    }

    #[test]
    fn modern_vite_alone_is_not_seeded() {
        let g = graph(&[
            ("app@1.0.0", "app", &[("vite", "8.1.3")]),
            ("vite@8.1.3", "vite", &[]),
        ]);
        let plan = expand(&g, &[]);
        assert!(
            plan.names.is_empty(),
            "vite>=8.1 needs no eject (Unit A covers it)"
        );
    }

    #[test]
    fn firebase_class_seeds_closure_and_hoists_the_undeclared_target() {
        // firebase → @firebase/database (imports the undeclared @firebase/app);
        // firebase also declares @firebase/app directly (transitive, not root).
        let g = graph(&[
            (
                "firebase@10.0.0",
                "firebase",
                &[("@firebase/database", "1.0.0"), ("@firebase/app", "0.15.0")],
            ),
            ("@firebase/database@1.0.0", "@firebase/database", &[]),
            ("@firebase/app@0.15.0", "@firebase/app", &[]),
        ]);
        let plan = expand(&g, &[]);
        let names: HashSet<&str> = plan.names.iter().map(String::as_str).collect();
        assert!(names.contains("@firebase/database"), "seed materialized");
        assert!(names.contains("firebase"), "importer in the closure");
        // Rung 2: the undeclared @firebase/app hoisted within @firebase/database.
        assert_eq!(
            plan.hoist_within
                .get("@firebase/database@1.0.0")
                .map(Vec::as_slice),
            Some(["@firebase/app@0.15.0".to_string()].as_slice())
        );
    }

    #[test]
    fn no_seeds_yields_empty_plan() {
        let g = graph(&[("lodash@4.17.21", "lodash", &[])]);
        let plan = expand(&g, &[]);
        assert!(plan.names.is_empty() && plan.hoist_within.is_empty());
    }
}
