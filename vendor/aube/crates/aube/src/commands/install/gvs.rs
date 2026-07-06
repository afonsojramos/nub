use crate::state;
use miette::miette;
use std::path::Path;

pub(super) fn resolve_global_virtual_store_override(
    settings_ctx: &aube_settings::ResolveCtx<'_>,
    manifests: &[(String, aube_manifest::PackageJson)],
    env_snapshot: &[(String, String)],
) -> Option<bool> {
    let explicit = aube_settings::resolved::enable_global_virtual_store(settings_ctx);
    explicit.or_else(|| {
        let triggers =
            aube_settings::resolved::disable_global_virtual_store_for_packages(settings_ctx);
        let triggered_by = super::settings::find_gvs_incompatible_trigger(manifests, &triggers);
        let ci_mode = env_snapshot.iter().any(|(k, _)| k == "CI");
        let virtual_store_only_setting = aube_settings::resolved::virtual_store_only(settings_ctx);
        if let Some(name) = triggered_by
            && !ci_mode
            && !virtual_store_only_setting
        {
            // The notice is unactionable by the end user (the only fix is an
            // upstream change in `{name}`), so an embedder that owns its own UX
            // (`gvs_incompatible_warning = false`) demotes it to `debug` —
            // silent at default verbosity, reachable when the engine log level
            // is raised. The per-project fallback (`Some(false)`) is identical
            // regardless; only the notice's level differs. The level can't be a
            // runtime value in `tracing::event!`, so the message is built once
            // and the macro selected by branch.
            let msg = format!(
                "`{name}` isn't compatible with aube's global virtual store — \
                 installing per-project instead. Install still succeeds; repeat \
                 installs of this project just won't share materialized packages \
                 across projects. Fixing this requires an upstream change in \
                 `{name}` itself (please file it with that project, not aube). \
                 To silence this warning, run `aube config set \
                 enableGlobalVirtualStore false --location project` — or set \
                 `disableGlobalVirtualStoreForPackages=[]` to opt out of this \
                 auto-detection entirely. \
                 Details: https://aube.jdx.dev/package-manager/global-virtual-store"
            );
            let code = aube_codes::warnings::WARN_AUBE_GVS_INCOMPATIBLE;
            if aube_util::embedder().gvs_incompatible_warning {
                tracing::warn!(code, "{msg}");
            } else {
                tracing::debug!(code, "{msg}");
            }
            Some(false)
        } else {
            None
        }
    })
}

pub(super) fn planned_global_virtual_store(
    use_global_virtual_store_override: Option<bool>,
    env_snapshot: &[(String, String)],
) -> bool {
    use_global_virtual_store_override
        .unwrap_or_else(|| !env_snapshot.iter().any(|(k, _)| k == "CI"))
}

/// How each isolated-layout `.aube/<dep_path>` entry is materialized on disk.
///
/// Makes the "hidden hoist tree inside the SHARED global store" state
/// UNREPRESENTABLE (aube issue #6: a bare-name alias inside the shared store is
/// cross-project-mutable, so a live shared store must never carry one). The
/// hidden tree is a field only of [`Disk`](Materialization::Disk) — the
/// project-local real-directory materialization; [`Symlink`](Materialization::Symlink),
/// which points into the shared store, carries none by construction. This
/// replaces the former loose `(effective_gvs, build_hidden_tree)` bool pair,
/// whose `(true, true)` combination was a representable contradiction.
///
/// The write METHOD ([`aube_linker::LinkStrategy`] = `package-import-method`)
/// and the LAYOUT ([`aube_linker::NodeLinker`] = `node-linker`) are separate,
/// already-orthogonal axes resolved independently — this type is only the
/// symlink-vs-disk × hidden-tree decision the `gvs_over_default_hoist` +
/// `hoist` + `enableGlobalVirtualStore` tangle used to fold into two bools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Materialization {
    /// Symlink into the shared global virtual store; files live machine-global.
    /// A hidden hoist tree can't live in a shared store, so there is none.
    Symlink,
    /// Project-local real directories (reflink/hardlink/copy from the CAS).
    /// `hidden_tree` builds the pnpm-parity `node_modules/.<store>/node_modules/`
    /// hidden hoist fallback.
    Disk { hidden_tree: bool },
}

impl Materialization {
    /// Resolve the materialization from the coupling inputs, folding the former
    /// `effective_global_virtual_store` + `build_hidden_tree` computation into
    /// one sound value.
    ///
    /// The shared store engages only on the ISOLATED layout with the requested
    /// mode on and nothing vetoing it: upstream (`gvs_over_default_hoist ==
    /// false`) ANY resolved `hoist=true` (the default) vetoes it; under the
    /// embedder profile (`true`) only an EXPLICITLY-set `hoist=true` does, so a
    /// DEFAULT hoist lets the store engage while the hidden hoist tree is built
    /// wherever it does not (CI, per-project, an incompatible-package trigger,
    /// an explicit opt-out, dlx). The `hoisted` layout discards `.aube`, so it
    /// never uses the shared store. `reset_on_mode_change` compares this
    /// against the existing `.aube/` tree, so it MUST predict the same value
    /// the linker writes (issue #71): a mismatch wipes `node_modules` on every
    /// non-fast-path install.
    pub(super) fn resolve(
        gvs_over_default_hoist: bool,
        planned_gvs: bool,
        resolved_hoist: bool,
        hoist_explicit: Option<bool>,
        node_linker: aube_linker::NodeLinker,
    ) -> Self {
        let isolated = matches!(node_linker, aube_linker::NodeLinker::Isolated);
        let hoist_vetoes_store = if gvs_over_default_hoist {
            hoist_explicit.unwrap_or(false)
        } else {
            resolved_hoist
        };
        if isolated && planned_gvs && !hoist_vetoes_store {
            Self::Symlink
        } else {
            Self::Disk {
                hidden_tree: resolved_hoist,
            }
        }
    }

    /// Whether the linker materializes `.aube/<dep>` as symlinks into the
    /// shared global virtual store (the former `effective_gvs`).
    pub(super) fn uses_shared_store(self) -> bool {
        matches!(self, Self::Symlink)
    }

    /// Whether the isolated linker builds the hidden hoist tree. Never true
    /// under [`Symlink`](Self::Symlink) — the constraint that motivates the type.
    pub(super) fn build_hidden_tree(self) -> bool {
        matches!(self, Self::Disk { hidden_tree: true })
    }
}

/// Reject the contradictory explicit request `enableGlobalVirtualStore=true`
/// together with a layout that structurally excludes the shared store — an
/// explicit `hoist=true` (which needs the project-local hidden tree) or
/// `node-linker=hoisted` (which discards `.aube` entirely). The old coupling
/// resolved this SILENTLY by dropping the shared-store request (hoist/hoisted
/// won); a two-sided explicit request is genuinely contradictory, so it is a
/// loud error at the config boundary instead. Codeless miette, matching the
/// adjacent `node-linker=pnp` rejection in `resolve_node_linker`. A DEFAULT
/// `hoist=true` is not a conflict — under the embedder profile it lets the
/// store engage — so only `hoist_explicit == Some(true)` triggers this.
pub(super) fn reject_gvs_layout_contradiction(
    enable_gvs_explicit: Option<bool>,
    hoist_explicit: Option<bool>,
    node_linker: aube_linker::NodeLinker,
) -> miette::Result<()> {
    if enable_gvs_explicit != Some(true) {
        return Ok(());
    }
    let conflicting = if matches!(node_linker, aube_linker::NodeLinker::Hoisted) {
        "node-linker=hoisted"
    } else if hoist_explicit == Some(true) {
        "hoist=true"
    } else {
        return Ok(());
    };
    Err(miette!(
        "enableGlobalVirtualStore=true conflicts with {conflicting}: the shared \
         global virtual store needs the isolated layout with no hidden hoist tree \
         (a shared-store bare-name alias would be cross-project-mutable state). \
         Set one, not both."
    ))
}

/// The global-virtual-store decision the fetch-pipelined materializer
/// (`spawn_gvs_prewarm`) must use so its per-package materialize lands
/// exactly where the link phase later reads it.
///
/// For the isolated linker this is the *effective* mode
/// ([`effective_global_virtual_store`]), which folds in `hoist`: under the
/// default `hoist=true` layout `Linker::link_all` recurses to
/// `without_global_virtual_store` and materializes per-project, so the
/// prewarm must too. Feeding the prewarm the raw (hoist-unaware) override
/// instead made it populate the shared global store while the link phase
/// re-materialized every package per-project off the fetch tail — wasted
/// work and a serial materialize the pipeline was meant to hide.
///
/// The hoisted linker discards `.aube` regardless, so it keeps the raw
/// override unchanged: forcing its prewarm per-project would only strand
/// orphan `.aube/<dep_path>` dirs the hoisted sweep doesn't reclaim.
pub(super) fn prewarm_global_virtual_store_override(
    node_linker: aube_linker::NodeLinker,
    effective_gvs: bool,
    raw_override: Option<bool>,
) -> Option<bool> {
    if matches!(node_linker, aube_linker::NodeLinker::Isolated) {
        Some(effective_gvs)
    } else {
        raw_override
    }
}

pub(super) fn reset_on_mode_change(
    cwd: &Path,
    aube_dir: &Path,
    modules_dir_name: &str,
    planned_gvs: bool,
) -> miette::Result<()> {
    let Some(existing_gvs) = super::settings::detect_aube_dir_gvs_mode(aube_dir) else {
        return Ok(());
    };
    if existing_gvs == planned_gvs {
        return Ok(());
    }

    let from = if existing_gvs { "enabled" } else { "disabled" };
    let to = if planned_gvs { "enabled" } else { "disabled" };
    let modules_dir_path = cwd.join(modules_dir_name);
    tracing::warn!(
        code = aube_codes::warnings::WARN_AUBE_GVS_MODE_CHANGED,
        "global virtual store {from} → {to}; removing {} and reinstalling from scratch",
        modules_dir_path.display()
    );
    remove_dir_all_if_exists(&modules_dir_path).map_err(|e| {
        miette!(
            "global virtual store transition: failed to remove {}: {e}",
            modules_dir_path.display()
        )
    })?;
    if !aube_dir.starts_with(&modules_dir_path) {
        remove_dir_all_if_exists(aube_dir).map_err(|e| {
            miette!(
                "global virtual store transition: failed to remove {}: {e}",
                aube_dir.display()
            )
        })?;
    }
    state::remove_state(cwd).map_err(|e| {
        miette!("global virtual store transition: failed to remove install state: {e}")
    })
}

fn remove_dir_all_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aube_linker::NodeLinker;

    // Regression for issue #71: the mode-change check must predict the
    // *effective* layout, which the linker forces to per-project whenever
    // the hidden hoist tree is on (the default) or the layout is hoisted.
    // Before the fix, `reset_on_mode_change` was fed the raw `planned_gvs`
    // (`true` off-CI), so a default project — whose linker materializes
    // per-project (`false`) — saw a spurious `disabled → enabled` flip on
    // every non-fast-path install (e.g. `add` in a workspace member),
    // wiping node_modules each time.

    // The stock (upstream) coupling: `gvs_over_default_hoist == false`, where a
    // default hoist=true still vetoes the shared store. Passing the flag
    // explicitly keeps these hermetic (no reliance on a process-global embedder).

    #[test]
    fn default_project_predicts_per_project_so_no_spurious_reset() {
        // hoist=true (default), isolated (default), store requested on (off-CI):
        // the linker writes per-project, so the effective mode is `false`.
        assert!(
            !Materialization::resolve(false, true, true, None, NodeLinker::Isolated)
                .uses_shared_store()
        );
    }

    #[test]
    fn store_active_only_when_hoist_off_and_isolated() {
        assert!(
            Materialization::resolve(false, true, false, Some(false), NodeLinker::Isolated)
                .uses_shared_store()
        );
    }

    #[test]
    fn hoisted_layout_never_uses_shared_store() {
        assert!(
            !Materialization::resolve(false, true, false, Some(false), NodeLinker::Hoisted)
                .uses_shared_store()
        );
        assert!(
            !Materialization::resolve(false, true, true, None, NodeLinker::Hoisted)
                .uses_shared_store()
        );
    }

    #[test]
    fn requested_off_stays_off_regardless_of_hoist() {
        assert!(
            !Materialization::resolve(false, false, false, Some(false), NodeLinker::Isolated)
                .uses_shared_store()
        );
        assert!(
            !Materialization::resolve(false, false, true, None, NodeLinker::Isolated)
                .uses_shared_store()
        );
    }

    // The `gvs_over_default_hoist` (nub) coupling, exercised through
    // `Materialization::resolve` with the flag passed explicitly (no
    // process-global embedder registration). Only an EXPLICIT hoist=true vetoes
    // the shared store. The asserted `uses_shared_store()` values are identical
    // to the former `effective_gvs_impl` outputs — behavior is preserved; the
    // enum only makes the illegal shared-store-plus-hidden-tree state
    // unrepresentable.

    #[test]
    fn flag_default_hoist_lets_store_engage() {
        // hoist resolves to the default `true` but was never explicitly set:
        // the shared store engages off-CI on the isolated layout.
        assert_eq!(
            Materialization::resolve(true, true, true, None, NodeLinker::Isolated),
            Materialization::Symlink
        );
    }

    #[test]
    fn flag_explicit_hoist_true_vetoes_store() {
        // An explicit hoist=true (e.g. nub's injected-deps embedder push) keeps
        // per-project + hidden tree, even off-CI.
        assert_eq!(
            Materialization::resolve(true, true, true, Some(true), NodeLinker::Isolated),
            Materialization::Disk { hidden_tree: true }
        );
    }

    #[test]
    fn flag_explicit_hoist_false_keeps_store_on() {
        assert_eq!(
            Materialization::resolve(true, true, false, Some(false), NodeLinker::Isolated),
            Materialization::Symlink
        );
    }

    #[test]
    fn flag_ci_and_hoisted_stay_off() {
        // planned off (CI) ⇒ off regardless of hoist explicitness.
        assert!(
            !Materialization::resolve(true, false, true, None, NodeLinker::Isolated)
                .uses_shared_store()
        );
        // hoisted layout never engages the shared store.
        assert!(
            !Materialization::resolve(true, true, true, None, NodeLinker::Hoisted)
                .uses_shared_store()
        );
    }

    #[test]
    fn hidden_tree_is_built_only_when_store_is_off() {
        // Default hoist, store on ⇒ no hidden tree (can't live in a shared store).
        assert!(
            !Materialization::resolve(true, true, true, None, NodeLinker::Isolated)
                .build_hidden_tree()
        );
        // Default hoist, store off (CI / per-project) ⇒ hidden tree built.
        assert!(
            Materialization::resolve(true, false, true, None, NodeLinker::Isolated)
                .build_hidden_tree()
        );
        // Explicit hoist=false ⇒ never a hidden tree.
        assert!(
            !Materialization::resolve(true, false, false, Some(false), NodeLinker::Isolated)
                .build_hidden_tree()
        );
    }

    #[test]
    fn symlink_never_builds_a_hidden_tree() {
        // The soundness invariant the type enforces: whenever the shared store
        // is live, no hidden tree — across every combination of inputs.
        for gvs_over_default_hoist in [false, true] {
            for planned_gvs in [false, true] {
                for resolved_hoist in [false, true] {
                    for hoist_explicit in [None, Some(false), Some(true)] {
                        for node_linker in [NodeLinker::Isolated, NodeLinker::Hoisted] {
                            let m = Materialization::resolve(
                                gvs_over_default_hoist,
                                planned_gvs,
                                resolved_hoist,
                                hoist_explicit,
                                node_linker,
                            );
                            assert!(
                                !(m.uses_shared_store() && m.build_hidden_tree()),
                                "shared store must never carry a hidden tree: {m:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    // The previously-SILENT contradiction (explicit enableGlobalVirtualStore=true
    // + a layout that structurally excludes the shared store) is now a loud
    // error at the config boundary.

    #[test]
    fn explicit_store_plus_explicit_hoist_is_a_loud_error() {
        assert!(
            reject_gvs_layout_contradiction(Some(true), Some(true), NodeLinker::Isolated).is_err()
        );
    }

    #[test]
    fn explicit_store_plus_hoisted_layout_is_a_loud_error() {
        assert!(reject_gvs_layout_contradiction(Some(true), None, NodeLinker::Hoisted).is_err());
    }

    #[test]
    fn store_with_default_hoist_or_no_explicit_request_is_not_a_conflict() {
        // Default hoist (None) + explicit store request: no conflict — the
        // profile lets a default hoist engage the store.
        assert!(reject_gvs_layout_contradiction(Some(true), None, NodeLinker::Isolated).is_ok());
        // No explicit store request: never a conflict, whatever the layout.
        assert!(reject_gvs_layout_contradiction(None, Some(true), NodeLinker::Isolated).is_ok());
        assert!(
            reject_gvs_layout_contradiction(Some(false), Some(true), NodeLinker::Hoisted).is_ok()
        );
    }

    // The prewarm materializer must mirror the link phase's *effective*
    // decision for the isolated linker so its per-package work lands where
    // `link_all` reads it. Under default isolated (`hoist=true`,
    // effective=false) it must route per-project; under GVS (`hoist=false`,
    // effective=true) it stays on the shared store.
    #[test]
    fn prewarm_isolated_mirrors_effective_gvs() {
        // default isolated (hoist=true) → effective false → per-project
        assert_eq!(
            prewarm_global_virtual_store_override(NodeLinker::Isolated, false, None),
            Some(false)
        );
        // GVS (hoist=false) → effective true → shared store
        assert_eq!(
            prewarm_global_virtual_store_override(NodeLinker::Isolated, true, None),
            Some(true)
        );
        // An explicit raw override is overridden by the effective decision:
        // the link phase ignores it once hoist forces per-project.
        assert_eq!(
            prewarm_global_virtual_store_override(NodeLinker::Isolated, false, Some(true)),
            Some(false)
        );
    }

    // The hoisted linker discards `.aube`, so its prewarm decision is left
    // as-is (the raw override) — never forced per-project, which would
    // strand orphan `.aube/<dep_path>` dirs the hoisted sweep can't reclaim.
    #[test]
    fn prewarm_hoisted_passes_raw_override_through() {
        assert_eq!(
            prewarm_global_virtual_store_override(NodeLinker::Hoisted, false, Some(true)),
            Some(true)
        );
        assert_eq!(
            prewarm_global_virtual_store_override(NodeLinker::Hoisted, false, None),
            None
        );
    }
}
