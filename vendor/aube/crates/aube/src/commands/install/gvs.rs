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

/// The global virtual store mode the linker will *actually* materialize.
///
/// `planned_global_virtual_store` is the requested mode (override, else
/// `!CI`), but the linker forces per-project materialization whenever the
/// hidden hoist tree is enabled (`hoist=true`, the default) or the layout
/// is `hoisted` — see `Linker::link_all`/`link_workspace`, both of which
/// fall back to `without_global_virtual_store()` under those conditions.
/// `reset_on_mode_change` compares this against the existing `.aube/` tree,
/// so it MUST predict the same value the linker writes; using the raw
/// `planned_gvs` instead made every non-fast-path install on a default
/// (`hoist=true`) project see a spurious `disabled → enabled` transition
/// and wipe `node_modules` (issue #71 — `aube add` in a workspace member,
/// which always bypasses the fast path).
///
/// Under the [`gvs_over_default_hoist`] embedder profile the coupling changes:
/// only an EXPLICITLY-set `hoist=true` (`hoist_explicit == Some(true)`) vetoes
/// the shared store, so a DEFAULT hoist lets GVS engage and the hidden hoist
/// tree is built only where GVS is off ([`build_hidden_tree`]). Standalone aube
/// (`gvs_over_default_hoist == false`) keeps the stock formula byte-for-byte, so
/// its lockstep with the linker's own `use_global_virtual_store && hoist`
/// fallback is preserved. Under the embedder profile the linker is instead
/// handed the pre-folded (effective GVS, hidden-tree) pair so that fallback is
/// never reached — see `run_link_phase`.
///
/// [`gvs_over_default_hoist`]: aube_util::Embedder::gvs_over_default_hoist
pub(super) fn effective_global_virtual_store(
    planned_gvs: bool,
    hoist: bool,
    hoist_explicit: Option<bool>,
    node_linker: aube_linker::NodeLinker,
) -> bool {
    effective_gvs_impl(
        aube_util::embedder().gvs_over_default_hoist,
        planned_gvs,
        hoist,
        hoist_explicit,
        node_linker,
    )
}

/// Pure core of [`effective_global_virtual_store`], the embedder flag passed
/// explicitly so both coupling regimes are unit-testable without registering a
/// process-global profile.
fn effective_gvs_impl(
    gvs_over_default_hoist: bool,
    planned_gvs: bool,
    hoist: bool,
    hoist_explicit: Option<bool>,
    node_linker: aube_linker::NodeLinker,
) -> bool {
    let isolated = matches!(node_linker, aube_linker::NodeLinker::Isolated);
    if gvs_over_default_hoist {
        planned_gvs && isolated && !hoist_explicit.unwrap_or(false)
    } else {
        planned_gvs && !hoist && isolated
    }
}

/// Whether the isolated linker should build the hidden hoist tree
/// (`node_modules/.<store>/node_modules/`). It exists ONLY where the shared
/// global virtual store is NOT active: a bare-name alias inside the SHARED
/// store is cross-project-mutable state (aube issue #6), so a live GVS must
/// never carry one. `resolved_hoist` is the resolved `hoist` setting (default
/// `true`); `effective_gvs` is [`effective_global_virtual_store`].
pub(super) fn build_hidden_tree(resolved_hoist: bool, effective_gvs: bool) -> bool {
    resolved_hoist && !effective_gvs
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

    // The stock (flag-off) coupling. Tests run with no embedder registered, so
    // `effective_global_virtual_store` reads `AUBE` (`gvs_over_default_hoist ==
    // false`) and a default hoist=true still vetoes the shared store.

    #[test]
    fn default_project_predicts_per_project_so_no_spurious_reset() {
        // hoist=true (default), isolated (default), GVS requested on (off-CI):
        // the linker writes per-project, so the effective mode is `false`.
        assert!(!effective_global_virtual_store(
            true,
            true,
            None,
            NodeLinker::Isolated
        ));
    }

    #[test]
    fn gvs_active_only_when_hoist_off_and_isolated() {
        assert!(effective_global_virtual_store(
            true,
            false,
            Some(false),
            NodeLinker::Isolated
        ));
    }

    #[test]
    fn hoisted_layout_never_uses_global_virtual_store() {
        assert!(!effective_global_virtual_store(
            true,
            false,
            Some(false),
            NodeLinker::Hoisted
        ));
        assert!(!effective_global_virtual_store(
            true,
            true,
            None,
            NodeLinker::Hoisted
        ));
    }

    #[test]
    fn requested_off_stays_off_regardless_of_hoist() {
        assert!(!effective_global_virtual_store(
            false,
            false,
            Some(false),
            NodeLinker::Isolated
        ));
        assert!(!effective_global_virtual_store(
            false,
            true,
            None,
            NodeLinker::Isolated
        ));
    }

    // The `gvs_over_default_hoist` (nub) coupling, exercised through the pure
    // `effective_gvs_impl` so the flag is set explicitly (no process-global
    // embedder registration). Only an EXPLICIT hoist=true vetoes GVS.

    #[test]
    fn flag_default_hoist_lets_gvs_engage() {
        // hoist resolves to the default `true` but was never explicitly set:
        // GVS engages off-CI on the isolated layout.
        assert!(effective_gvs_impl(
            true,
            true,
            true,
            None,
            NodeLinker::Isolated
        ));
    }

    #[test]
    fn flag_explicit_hoist_true_vetoes_gvs() {
        // An explicit hoist=true (e.g. nub's injected-deps embedder push) keeps
        // per-project + hidden tree, even off-CI.
        assert!(!effective_gvs_impl(
            true,
            true,
            true,
            Some(true),
            NodeLinker::Isolated
        ));
    }

    #[test]
    fn flag_explicit_hoist_false_keeps_gvs_on() {
        assert!(effective_gvs_impl(
            true,
            true,
            false,
            Some(false),
            NodeLinker::Isolated
        ));
    }

    #[test]
    fn flag_ci_and_hoisted_stay_off() {
        // planned off (CI) ⇒ off regardless of hoist explicitness.
        assert!(!effective_gvs_impl(
            true,
            false,
            true,
            None,
            NodeLinker::Isolated
        ));
        // hoisted layout never engages the shared store.
        assert!(!effective_gvs_impl(
            true,
            true,
            true,
            None,
            NodeLinker::Hoisted
        ));
    }

    #[test]
    fn hidden_tree_is_built_only_when_gvs_is_off() {
        // Default hoist, GVS on ⇒ no hidden tree (can't live in a shared store).
        assert!(!build_hidden_tree(true, true));
        // Default hoist, GVS off (CI / per-project) ⇒ hidden tree built.
        assert!(build_hidden_tree(true, false));
        // Explicit hoist=false ⇒ never a hidden tree.
        assert!(!build_hidden_tree(false, false));
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
