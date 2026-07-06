//! Disk-materialize plan expansion — the embedder-pluggable seam that turns a
//! flat seed list into a graph-aware selective-subtree materialization plan.
//!
//! Standalone aube installs NO hook, so [`expand_disk_materialize`] returns the
//! seed verbatim (rung-1 names = the seed, no rung-2 hoist) and the linker's
//! disk-materialize pass is byte-for-byte unchanged. An embedder (nub) installs
//! a hook via [`set_disk_materialize_expand_hook`] that consults the resolved
//! graph to (1) expand each seed to its ancestor-closure — every package that
//! transitively imports it — so a transitively-consumed package materializes
//! together with its importers (else a store-resident importer resolves the
//! un-materialized copy, a silent singleton split), and (2) compute per-package
//! phantom-target hoists for undeclared imports the closure alone can't place.
//!
//! The hook receives only `&LockfileGraph` + the seed names and returns a pure
//! [`DiskMaterializePlan`], so all embedder-specific policy (which packages
//! seed, the flag gate, vite<8.1 detection) lives in the embedder; aube owns
//! only the neutral seam and the graph primitive ([`LockfileGraph::importer_closure`]).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use aube_lockfile::LockfileGraph;

/// The materialization plan a [`DmExpandHook`] produces from the resolved graph.
#[derive(Debug, Default, Clone)]
pub struct DiskMaterializePlan {
    /// Rung 1 — the expanded set of package NAMES to disk-materialize
    /// project-local (the seed UNION its ancestor-closure). Fed to
    /// [`Linker::with_disk_materialize`](crate::Linker::with_disk_materialize),
    /// matched by exact name, exactly as the raw seed was.
    pub names: Vec<String>,
    /// Rung 2 — undeclared phantom targets to hoist-within a disk-materialized
    /// package's own `node_modules`, keyed by the importer's dep_path → the
    /// already-resolved target dep_paths. Empty unless a hook populates it.
    pub hoist_within: BTreeMap<String, Vec<String>>,
}

/// A hook that expands a disk-materialize seed into a graph-aware plan. `Send +
/// Sync` because it is stored in a process-global consulted from the install
/// pipeline; `'static` because it outlives any single install.
pub type DmExpandHook =
    Box<dyn Fn(&LockfileGraph, &[String]) -> DiskMaterializePlan + Send + Sync + 'static>;

static DM_EXPAND_HOOK: OnceLock<DmExpandHook> = OnceLock::new();

/// Install the embedder's disk-materialize expansion hook. Set-once: a second
/// call is ignored (the first registration wins), matching aube's other
/// process-global embedder seams. Called once at engine-session build; standalone
/// aube never calls it, so the default path stays hook-free.
pub fn set_disk_materialize_expand_hook(hook: DmExpandHook) {
    let _ = DM_EXPAND_HOOK.set(hook);
}

/// Expand a disk-materialize `seed` (the resolved `diskMaterializePackages`
/// names) into a plan against `graph`. With no hook installed — standalone aube,
/// every test — returns the seed verbatim as rung-1 names with an empty rung-2
/// map, so the caller's `with_disk_materialize(&plan.names)` is identical to the
/// pre-existing `with_disk_materialize(&seed)` and nothing else changes.
pub fn expand_disk_materialize(graph: &LockfileGraph, seed: &[String]) -> DiskMaterializePlan {
    match DM_EXPAND_HOOK.get() {
        Some(hook) => hook(graph, seed),
        None => DiskMaterializePlan {
            names: seed.to_vec(),
            hoist_within: BTreeMap::new(),
        },
    }
}
