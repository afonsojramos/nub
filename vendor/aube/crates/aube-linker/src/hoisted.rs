//! Hoisted (`node-linker=hoisted`) layout.
//!
//! Unlike the isolated layout — which materializes every package under
//! a per-project `.aube/<dep_path>/` virtual store and builds Node's
//! module graph out of symlinks — the hoisted layout writes real
//! package directories straight into `node_modules/`, nesting
//! conflicting versions under the parent that requires them. This
//! matches npm / yarn-classic's flat tree and is what certain legacy
//! toolchains (React Native's Metro, some Jest plugins) require.
//!
//! Placement algorithm (npm-style, per importer):
//!
//! 1. Start with a `TreeNode` for the importer — its `node_modules`
//!    directory and an empty child map.
//! 2. BFS from the importer's direct deps. For each `(requester, name,
//!    dep_path)` pair, walk up from the requester looking for the
//!    shallowest ancestor whose `children[name]` is either absent or
//!    points at the same `dep_path`. That ancestor becomes the
//!    placement site.
//! 3. If a matching entry already exists at that ancestor, reuse it
//!    (dedupe). Otherwise create a new child node and enqueue every
//!    transitive dep of the placed package with the new node as
//!    requester.
//! 4. Conflicting versions naturally nest: when walking up from the
//!    requester we stop as soon as we find a different `dep_path`
//!    under the same name, so the conflict forces the new entry to
//!    live below the blocker (typically inside the requester's own
//!    `node_modules/`).
//!
//! The planner operates purely on dep_path strings — the same keys
//! aube-lockfile uses — so peer-context dep_paths like
//! `react-router@6(react@18)` are treated as distinct and won't
//! collapse onto a plain `react-router@6` placement. The side effect
//! is that peer-variant conflicts nest deeper in hoisted mode than in
//! isolated mode, which is the correct-but-slightly-inefficient
//! fallback.
//!
//! The planner output (`PlacementPlan`) is consumed by the
//! materializer in `link_hoisted_importer` and also surfaced to the
//! install driver via `HoistedPlacements` so bin linking and
//! dependency lifecycle scripts can locate a package's on-disk
//! directory without recomputing the tree.

use crate::pool::with_link_pool;
use crate::{Error, HoistingLimits, LinkStats, Linker, apply_multi_file_patch};
use aube_lockfile::{DirectDep, LocalSource, LockedPackage, LockfileGraph};
use aube_store::PackageIndex;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// Map from lockfile `dep_path` to the absolute on-disk directories
/// where that package ended up. Most entries have exactly one path;
/// packages whose name conflicts with a shallower version end up
/// duplicated across multiple parent `node_modules/` directories so
/// each gets its own on-disk copy.
#[derive(Debug, Default, Clone)]
pub struct HoistedPlacements {
    by_dep_path: BTreeMap<String, Vec<PathBuf>>,
}

impl HoistedPlacements {
    /// Recompute hoisted placement paths for an already-linked graph
    /// without touching disk. Used by commands like `aube rebuild`
    /// that need to find package directories after install, but must
    /// not relink node_modules. `modules_dir_name` must match the
    /// `modulesDir` setting the install used, or the computed paths
    /// won't match what's on disk.
    pub fn from_graph(
        root_dir: &Path,
        graph: &LockfileGraph,
        modules_dir_name: &str,
        hoisting_limits: HoistingLimits,
    ) -> Result<Self, Error> {
        let mut placements = Self::default();
        for (importer_path, deps) in &graph.importers {
            if !crate::is_physical_importer(importer_path) {
                continue;
            }
            let importer_dir = if importer_path == "." {
                root_dir.to_path_buf()
            } else {
                root_dir.join(importer_path)
            };
            let nm = importer_dir.join(modules_dir_name);
            let plan = plan_importer(&nm, deps, graph, hoisting_limits)?;
            for node in &plan.nodes {
                let (Some(dep_path), Some(pkg_dir)) = (&node.dep_path, &node.pkg_dir) else {
                    continue;
                };
                if pkg_dir.exists() {
                    placements.record(dep_path, pkg_dir.clone());
                }
            }
        }
        Ok(placements)
    }

    /// Shallowest placement for `dep_path`, or `None` if the dep is
    /// not in the hoisted tree (e.g. filtered by `--prod` /
    /// `--no-optional`). Used by the install driver as the canonical
    /// location for bin linking and lifecycle-script cwds.
    pub fn package_dir(&self, dep_path: &str) -> Option<&Path> {
        self.by_dep_path
            .get(dep_path)
            .and_then(|v| v.first())
            .map(|p| p.as_path())
    }

    /// Every placement site for `dep_path`. When a name conflicts
    /// with a shallower version the same dep_path may appear at
    /// multiple depths; lifecycle scripts run once per site so each
    /// copy has its native-build artifacts in place.
    pub fn all_package_dirs(&self, dep_path: &str) -> &[PathBuf] {
        self.by_dep_path
            .get(dep_path)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Iterate `(dep_path, placement_path)` pairs in BTree order.
    /// Primarily used by the top-level installer when it wants to
    /// walk every placed copy (e.g. the stale-directory sweep or the
    /// lifecycle-script dispatcher).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.by_dep_path
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |p| (k.as_str(), p.as_path())))
    }

    pub(crate) fn record(&mut self, dep_path: &str, path: PathBuf) {
        self.by_dep_path
            .entry(dep_path.to_string())
            .or_default()
            .push(path);
    }
}

/// One node in the placement tree. A node is either the importer
/// root (`pkg_dir == None`) or a placed package. `nm_dir` is the
/// `node_modules/` directory underneath this node where its children
/// live — for the importer that's `<importer>/node_modules`, for a
/// placed package it's `<parent.nm_dir>/<name>/node_modules`.
struct TreeNode {
    pkg_dir: Option<PathBuf>,
    nm_dir: PathBuf,
    parent: Option<usize>,
    children: BTreeMap<String, usize>,
    dep_path: Option<String>,
}

/// Arena-backed placement tree.
pub(crate) struct PlacementPlan {
    nodes: Vec<TreeNode>,
    root_idx: usize,
}

struct PlaceOutcome {
    node_idx: usize,
    created: bool,
}

impl PlacementPlan {
    fn new(importer_nm: PathBuf) -> Self {
        let root = TreeNode {
            pkg_dir: None,
            nm_dir: importer_nm,
            parent: None,
            children: BTreeMap::new(),
            dep_path: None,
        };
        Self {
            nodes: vec![root],
            root_idx: 0,
        }
    }

    /// Place `(name, dep_path)` under the ancestor chain rooted at
    /// `requester`. Returns the resulting node index and whether a
    /// fresh entry was created (so the caller knows whether to
    /// enqueue transitive deps).
    fn place(
        &mut self,
        requester: usize,
        floor: usize,
        name: &str,
        dep_path: &str,
    ) -> Result<PlaceOutcome, Error> {
        crate::validate_package_link_name(name)?;
        debug_assert!(is_ancestor_or_self(&self.nodes, floor, requester));
        // Reuse a matching package anywhere already visible through
        // Node's ancestor lookup, even if the hoist limit would
        // prevent placing a new package that high.
        let mut cursor = requester;
        loop {
            if let Some(&existing) = self.nodes[cursor].children.get(name) {
                if self.nodes[existing].dep_path.as_deref() == Some(dep_path) {
                    return Ok(PlaceOutcome {
                        node_idx: existing,
                        created: false,
                    });
                }
                // A nearer same-name package blocks Node from
                // resolving to any matching package above it.
                break;
            }
            match self.nodes[cursor].parent {
                Some(p) => cursor = p,
                None => break,
            }
        }

        // Walk up from the requester looking for the shallowest
        // allowed ancestor that doesn't already host a different
        // version of `name`.
        let mut cursor = requester;
        let mut candidate = requester;
        loop {
            if self.nodes[cursor].children.contains_key(name) {
                // Conflict: must stay at or below `candidate`.
                break;
            }
            candidate = cursor;
            if cursor == floor {
                break;
            }
            match self.nodes[cursor].parent {
                Some(p) => cursor = p,
                None => break,
            }
        }

        let parent_nm = self.nodes[candidate].nm_dir.clone();
        let pkg_dir = parent_nm.join(name);
        let nm_dir = pkg_dir.join("node_modules");
        let new_idx = self.nodes.len();
        self.nodes.push(TreeNode {
            pkg_dir: Some(pkg_dir),
            nm_dir,
            parent: Some(candidate),
            children: BTreeMap::new(),
            dep_path: Some(dep_path.to_string()),
        });
        self.nodes[candidate]
            .children
            .insert(name.to_string(), new_idx);
        Ok(PlaceOutcome {
            node_idx: new_idx,
            created: true,
        })
    }

    /// Names placed directly in the importer root's `node_modules/`.
    /// Drives the stale-entry sweep in `link_hoisted_importer`.
    pub(crate) fn root_names(&self) -> impl Iterator<Item = &str> {
        self.nodes[self.root_idx]
            .children
            .keys()
            .map(|s| s.as_str())
    }
}

fn is_ancestor_or_self(nodes: &[TreeNode], ancestor: usize, mut node: usize) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        let Some(parent) = nodes[node].parent else {
            return false;
        };
        node = parent;
    }
}

/// Build a placement plan for a single importer.
pub(crate) fn plan_importer(
    importer_nm: &Path,
    root_deps: &[DirectDep],
    graph: &LockfileGraph,
    hoisting_limits: HoistingLimits,
) -> Result<PlacementPlan, Error> {
    let mut plan = PlacementPlan::new(importer_nm.to_path_buf());
    let mut queue: VecDeque<(usize, usize, String, String)> = VecDeque::new();

    // Seed the queue with the importer's direct deps in declaration
    // order. BFS makes shallower deps win placement ties over
    // deeper ones, which matches npm's first-writer-wins policy.
    for dep in root_deps {
        if !graph.packages.contains_key(&dep.dep_path) {
            continue;
        }
        queue.push_back((
            plan.root_idx,
            plan.root_idx,
            dep.name.clone(),
            dep.dep_path.clone(),
        ));
    }

    while let Some((requester, floor, name, dep_path)) = queue.pop_front() {
        let outcome = plan.place(requester, floor, &name, &dep_path)?;
        if !outcome.created {
            continue;
        }
        let Some(pkg) = graph.packages.get(&dep_path) else {
            continue;
        };
        // Skip transitives for `link:` deps — their target directory
        // holds its own node_modules and Node resolves through it
        // naturally. Materializing a copy would fight with a live
        // workspace package.
        if matches!(pkg.local_source.as_ref(), Some(LocalSource::Link(_))) {
            continue;
        }
        let child_floor = match hoisting_limits {
            HoistingLimits::None | HoistingLimits::Workspaces => plan.root_idx,
            HoistingLimits::Dependencies => outcome.node_idx,
        };
        for (dep_name, dep_tail) in &pkg.dependencies {
            // Git / remote-tarball deps are recorded by their resolved URL
            // spec but keyed under the short `name@git+<hash>` /
            // `name@url+<hash>` form, so the verbatim `name@tail` key would
            // miss `graph.packages` and silently drop the dep's subtree.
            let child_dep_path = aube_lockfile::shared_local_dep_path(dep_name, dep_tail)
                .unwrap_or_else(|| format!("{dep_name}@{dep_tail}"));
            if !graph.packages.contains_key(&child_dep_path) {
                continue;
            }
            queue.push_back((
                outcome.node_idx,
                child_floor,
                dep_name.clone(),
                child_dep_path,
            ));
        }
    }

    Ok(plan)
}

/// Materialize a planned tree onto disk for a single importer.
///
/// Called by `Linker::link_all` and `Linker::link_workspace` when the
/// linker is configured with `NodeLinker::Hoisted`. The importer's
/// existing `node_modules/` is swept of any top-level entries the
/// plan doesn't claim (direct deps from a previous install may have
/// changed); placed packages are then materialized in two passes —
/// local (`file:`/`link:`) first, then registry packages via the
/// standard reflink/hardlink/copy file-linker.
///
/// Every placed package is recorded in `placements` so the install
/// driver can later resolve `dep_path -> on-disk dir` for bin
/// linking and lifecycle scripts without recomputing the plan.
pub(crate) struct HoistedImporterDirs<'a> {
    pub(crate) root: &'a Path,
    pub(crate) importer: &'a Path,
}

/// What one placement node contributed to the importer-wide totals.
/// Returned by the parallel per-node materializer so the serial caller
/// can fold stats and placement sites in deterministic node order —
/// `placements` is a single `(dep_path, pkg_dir)` for a materialized or
/// `link:` node, empty only on the can't-happen skip paths.
struct NodeOutcome {
    stats: LinkStats,
    placement: Option<(String, PathBuf)>,
}

/// Materialize ONE placement node onto disk. Pure per-node work — it
/// writes only inside `pkg_dir` (or the symlink at `pkg_dir` for a
/// `link:` dep), so nodes at the same placement depth run concurrently
/// without touching each other's directories. Destructive ordering
/// across depths (a parent that ships a bundled `node_modules/<dep>` is
/// wiped + replaced by the deeper real placement) is the caller's
/// responsibility, enforced by a barrier between depth levels.
fn materialize_hoisted_node(
    linker: &Linker,
    dep_path: &str,
    pkg_dir: PathBuf,
    pkg: &LockedPackage,
    package_indices: &BTreeMap<String, PackageIndex>,
    root_dir: &Path,
    nm: &Path,
) -> Result<NodeOutcome, Error> {
    let mut stats = LinkStats::default();

    // `link:` dep: symlink the package dir straight at the target.
    // `link:` packages were excluded from the dependency plan (their
    // target owns its deps); `portal:` stays on the materialized path.
    // Counting toward `top_level_linked` is the caller's job (it adds
    // the root's child count once), so no stat bump here.
    if let Some(LocalSource::Link(rel)) = pkg.local_source.as_ref() {
        if let Some(parent) = pkg_dir.parent() {
            crate::mkdirp(parent)?;
        }
        crate::try_remove_entry(&pkg_dir);
        let abs_target = root_dir.join(rel);
        let link_parent = pkg_dir.parent().unwrap_or(nm);
        let rel_target = pathdiff::diff_paths(&abs_target, link_parent).unwrap_or(abs_target);
        crate::sys::create_dir_link(&rel_target, &pkg_dir)
            .map_err(|e| Error::Io(pkg_dir.clone(), e))?;
        return Ok(NodeOutcome {
            stats,
            placement: Some((dep_path.to_string(), pkg_dir)),
        });
    }

    // Registry (or `file:`) package — needs a PackageIndex to find the
    // store-backed file set. `package_indices` is sparse on warm
    // installs, so lazy-load from the store on miss. `registry_name()`
    // is the lookup key for npm-aliased packages, and integrity is part
    // of the cache key so a same-name dep from a different source can't
    // pick up a registry entry's file list.
    let owned_index;
    let index = match package_indices.get(dep_path) {
        Some(i) => i,
        None => {
            owned_index = linker
                .store
                .load_index(pkg.registry_name(), &pkg.version, linker.index_read_key(pkg))
                .ok_or_else(|| Error::MissingPackageIndex(dep_path.to_string()))?;
            &owned_index
        }
    };

    // Wipe any previous contents (a version change, or a bundled copy
    // a shallower package shipped at this path) so stale files don't
    // survive. Must precede the clone: `clonefile(2)` requires its
    // destination not pre-exist, and the per-file fallback wants a clean
    // dir too.
    crate::try_remove_entry(&pkg_dir);

    // Whole-dir `clonefile(2)` fast path (macOS+APFS, same volume) —
    // the identical primitive the isolated linker uses. The hoisted
    // per-file fill is bound by APFS serializing file creation, so
    // parallelizing it barely moves the needle on macOS; cloning the
    // package's extracted-tree tier in ONE syscall replaces the entire
    // per-file loop and is what makes this layout fast. The tree holds
    // exactly the package's own files (no transitive symlinks — hoisted
    // nests real dirs instead), so the clone reproduces the per-file
    // result byte-for-byte, +x bits included. Any miss (tier unbuilt,
    // non-APFS, cross-volume, non-macOS, or a clone error) returns false
    // and falls through to the unchanged per-file path below.
    let tree_key = linker.virtual_store_subdir(dep_path);
    let tree_src = linker.store.tree_path(&tree_key);
    let pkg_nm_parent = pkg_dir.parent().unwrap_or(&pkg_dir).to_path_buf();
    let used_clonedir = linker.try_clonedir_fill(
        &pkg_dir,
        &pkg_nm_parent,
        &tree_src,
        dep_path,
        pkg,
        index,
        &mut stats,
    )?;

    if !used_clonedir {
        // Per-file fallback: batch-create every intermediate parent
        // directory in one pass, then link each file.
        let mut parents: BTreeSet<PathBuf> = BTreeSet::new();
        parents.insert(pkg_dir.clone());
        for rel_path in index.keys() {
            crate::validate_index_key(rel_path)?;
            let target = pkg_dir.join(rel_path);
            if let Some(parent) = target.parent() {
                parents.insert(parent.to_path_buf());
            }
        }
        for parent in &parents {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io(parent.clone(), e))?;
        }

        for (rel_path, stored) in index {
            // Key already validated in the parent-collection loop above;
            // the index is immutable between the two.
            let target = pkg_dir.join(rel_path);
            if let Err(e) = linker.link_file_fresh(stored, rel_path, &target) {
                if let Error::MissingStoreFile { .. } = &e {
                    crate::invalidate_stale_index_for_package(
                        &linker.store,
                        pkg,
                        linker.index_read_key(pkg),
                    );
                }
                return Err(e);
            }
            stats.files_linked += 1;
            if stored.executable {
                #[cfg(unix)]
                xx::file::make_executable(&target).map_err(|e| Error::Xx(e.to_string()))?;
            }
        }
    }

    let patch_key = pkg.spec_key();
    if let Some(patch_text) = linker.patches.get(&patch_key) {
        apply_multi_file_patch(&pkg_dir, patch_text)
            .map_err(|msg| Error::Patch(patch_key.clone(), msg))?;
    }

    stats.packages_linked += 1;
    Ok(NodeOutcome {
        stats,
        placement: Some((dep_path.to_string(), pkg_dir)),
    })
}

pub(crate) fn link_hoisted_importer(
    linker: &Linker,
    dirs: HoistedImporterDirs<'_>,
    root_deps: &[DirectDep],
    graph: &LockfileGraph,
    package_indices: &BTreeMap<String, PackageIndex>,
    stats: &mut LinkStats,
    placements: &mut HoistedPlacements,
) -> Result<(), Error> {
    let root_dir = dirs.root;
    let importer_dir = dirs.importer;
    let nm = importer_dir.join(linker.modules_dir_name());
    crate::mkdirp(&nm)?;

    let plan = plan_importer(&nm, root_deps, graph, linker.hoisting_limits)?;

    // Sweep any top-level entries that are no longer claimed by the
    // plan. Dotfiles (`.aube`, `.bin`, …) are preserved — .aube in
    // particular may hold a previous isolated tree that the user
    // hasn't switched off; we leave it alone rather than wiping
    // bytes the other layout owns.
    let keep_root: std::collections::HashSet<&str> = plan.root_names().collect();
    crate::sweep_stale_top_level_entries(&nm, &keep_root, None);

    // Materialize every non-root node, parallelized across packages.
    //
    // Correctness hinges on ONE ordering rule: a package may ship a
    // bundled `node_modules/<dep>` inside its own tarball, and the real
    // deeper placement of `<dep>` then wipes + replaces it (child wins).
    // So a node must be fully materialized before any DESCENDANT's
    // destructive wipe + fill runs. We get that for free by processing
    // the placement tree one depth level at a time: BFS gives every
    // child a higher arena index than its parent, so `depth[idx]` folds
    // in a single forward pass, and within a level every node owns a
    // disjoint directory subtree (no two same-depth nodes are
    // ancestor/descendant), so their fills never collide. The
    // `collect()` after each level is the barrier between depths.
    //
    // Placements are folded back in level-then-index order — identical
    // to the old serial BFS order — so `package_dir()` still returns the
    // shallowest site for a dep duplicated across nested `node_modules`.
    let mut depth = vec![0usize; plan.nodes.len()];
    let mut max_depth = 0usize;
    for idx in 0..plan.nodes.len() {
        if let Some(parent) = plan.nodes[idx].parent {
            depth[idx] = depth[parent] + 1;
            max_depth = max_depth.max(depth[idx]);
        }
    }

    let parallelism = linker.link_parallelism();
    for level in 1..=max_depth {
        // Serial prep: clone the (dep_path, pkg_dir) each node needs and
        // resolve its `LockedPackage`. Cheap; the expensive file I/O runs
        // in the par_iter below.
        let tasks: Vec<(String, PathBuf, &LockedPackage)> = (0..plan.nodes.len())
            .filter(|&idx| depth[idx] == level)
            .filter_map(|idx| {
                let node = &plan.nodes[idx];
                let (Some(dep_path), Some(pkg_dir)) = (&node.dep_path, &node.pkg_dir) else {
                    return None;
                };
                let pkg = graph.packages.get(dep_path)?;
                Some((dep_path.clone(), pkg_dir.clone(), pkg))
            })
            .collect();
        if tasks.is_empty() {
            continue;
        }

        let results: Vec<Result<NodeOutcome, Error>> = with_link_pool(parallelism, || {
            use rayon::prelude::*;
            tasks
                .par_iter()
                .map(|(dep_path, pkg_dir, pkg)| {
                    materialize_hoisted_node(
                        linker,
                        dep_path,
                        pkg_dir.clone(),
                        pkg,
                        package_indices,
                        root_dir,
                        &nm,
                    )
                })
                .collect()
        });

        for result in results {
            let outcome = result?;
            stats.files_linked += outcome.stats.files_linked;
            stats.packages_linked += outcome.stats.packages_linked;
            if let Some((dep_path, pkg_dir)) = outcome.placement {
                placements.record(&dep_path, pkg_dir);
            }
        }
    }

    stats.top_level_linked += plan.nodes[plan.root_idx].children.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aube_lockfile::{DepType, LockedPackage};

    fn dep(name: &str, dep_path: &str) -> DirectDep {
        DirectDep {
            name: name.to_string(),
            dep_path: dep_path.to_string(),
            dep_type: DepType::Production,
            specifier: None,
        }
    }

    fn pkg(name: &str, version: &str, deps: &[(&str, &str)]) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            dep_path: format!("{name}@{version}"),
            dependencies: deps
                .iter()
                .map(|(dep_name, tail)| ((*dep_name).to_string(), (*tail).to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn package_dir(plan: &PlacementPlan, dep_path: &str) -> PathBuf {
        plan.nodes
            .iter()
            .find(|node| node.dep_path.as_deref() == Some(dep_path))
            .and_then(|node| node.pkg_dir.clone())
            .unwrap_or_else(|| panic!("{dep_path} was not placed"))
    }

    #[test]
    fn dependencies_limit_keeps_transitives_under_their_direct_dep() {
        let nm = PathBuf::from("/project/node_modules");
        let mut graph = LockfileGraph::default();
        graph.packages.insert(
            "app@1.0.0".into(),
            pkg("app", "1.0.0", &[("left-pad", "1.0.0")]),
        );
        graph.packages.insert(
            "left-pad@1.0.0".into(),
            pkg("left-pad", "1.0.0", &[("repeat", "1.0.0")]),
        );
        graph
            .packages
            .insert("repeat@1.0.0".into(), pkg("repeat", "1.0.0", &[]));
        let root_deps = vec![dep("app", "app@1.0.0")];

        let unlimited = plan_importer(&nm, &root_deps, &graph, HoistingLimits::None).unwrap();
        assert_eq!(
            package_dir(&unlimited, "left-pad@1.0.0"),
            nm.join("left-pad")
        );
        assert_eq!(package_dir(&unlimited, "repeat@1.0.0"), nm.join("repeat"));

        let limited = plan_importer(&nm, &root_deps, &graph, HoistingLimits::Dependencies).unwrap();
        assert_eq!(
            package_dir(&limited, "left-pad@1.0.0"),
            nm.join("app/node_modules/left-pad")
        );
        assert_eq!(
            package_dir(&limited, "repeat@1.0.0"),
            nm.join("app/node_modules/left-pad/node_modules/repeat")
        );
    }

    #[test]
    fn dependencies_limit_reuses_matching_direct_dependency_above_floor() {
        let nm = PathBuf::from("/project/node_modules");
        let mut graph = LockfileGraph::default();
        graph.packages.insert(
            "app@1.0.0".into(),
            pkg("app", "1.0.0", &[("shared", "1.0.0")]),
        );
        graph
            .packages
            .insert("shared@1.0.0".into(), pkg("shared", "1.0.0", &[]));
        let root_deps = vec![dep("shared", "shared@1.0.0"), dep("app", "app@1.0.0")];

        let limited = plan_importer(&nm, &root_deps, &graph, HoistingLimits::Dependencies).unwrap();

        assert_eq!(package_dir(&limited, "shared@1.0.0"), nm.join("shared"));
        assert_eq!(
            limited
                .nodes
                .iter()
                .filter(|node| node.dep_path.as_deref() == Some("shared@1.0.0"))
                .count(),
            1
        );
    }

    #[test]
    fn dependencies_limit_does_not_reuse_above_version_blocker() {
        let nm = PathBuf::from("/project/node_modules");
        let mut graph = LockfileGraph::default();
        graph.packages.insert(
            "app@1.0.0".into(),
            pkg("app", "1.0.0", &[("shared", "2.0.0"), ("tool", "1.0.0")]),
        );
        graph.packages.insert(
            "tool@1.0.0".into(),
            pkg("tool", "1.0.0", &[("shared", "1.0.0")]),
        );
        graph
            .packages
            .insert("shared@1.0.0".into(), pkg("shared", "1.0.0", &[]));
        graph
            .packages
            .insert("shared@2.0.0".into(), pkg("shared", "2.0.0", &[]));
        let root_deps = vec![dep("shared", "shared@1.0.0"), dep("app", "app@1.0.0")];

        let limited = plan_importer(&nm, &root_deps, &graph, HoistingLimits::Dependencies).unwrap();

        let shared_v1_dirs: Vec<_> = limited
            .nodes
            .iter()
            .filter(|node| node.dep_path.as_deref() == Some("shared@1.0.0"))
            .filter_map(|node| node.pkg_dir.as_ref())
            .collect();
        assert_eq!(shared_v1_dirs.len(), 2);
        assert!(shared_v1_dirs.contains(&&nm.join("shared")));
        assert!(shared_v1_dirs.contains(&&nm.join("app/node_modules/tool/node_modules/shared")));
    }

    #[test]
    fn from_graph_respects_dependencies_limit() {
        let root = tempfile::tempdir().unwrap();
        let nm = root.path().join("node_modules");
        let app_dir = nm.join("app");
        let left_pad_dir = app_dir.join("node_modules/left-pad");
        std::fs::create_dir_all(&left_pad_dir).unwrap();

        let mut graph = LockfileGraph::default();
        graph
            .importers
            .insert(".".into(), vec![dep("app", "app@1.0.0")]);
        graph.packages.insert(
            "app@1.0.0".into(),
            pkg("app", "1.0.0", &[("left-pad", "1.0.0")]),
        );
        graph
            .packages
            .insert("left-pad@1.0.0".into(), pkg("left-pad", "1.0.0", &[]));

        let placements = HoistedPlacements::from_graph(
            root.path(),
            &graph,
            "node_modules",
            HoistingLimits::Dependencies,
        )
        .unwrap();

        assert_eq!(
            placements.package_dir("left-pad@1.0.0"),
            Some(left_pad_dir.as_path())
        );
    }
}
