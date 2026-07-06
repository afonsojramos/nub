//! Dynamic per-version phantom scan → disk-eject (the default; the
//! `NUB_DYNAMIC_PHANTOM_EJECT=0` opt-out disables it).
//!
//! This REPLACES the old hand-curated static force-materialize list (capped at a
//! corpus, stale on new versions) by SCANNING each installed dependency
//! version's real published code: does it, along its reachable
//! `exports`/`main`/`bin` graph, statically and unguardedly import a package it
//! does not declare? A version's code is immutable, so the verdict is computed
//! once per content-fingerprint and cached machine-wide.
//!
//! Placement — EXTRACT TIME, not post-link. The scan is registered as an aube
//! store extract hook ([`aube_store::set_extract_hook`]) that fires at the end of
//! each tarball import, on the fetch/blocking fan-out thread, so per-version
//! analysis OVERLAPS the network-bound fetch phase (the scan CPU hides under
//! fetch's idle cores) instead of adding a serial post-link pass. Each verdict is
//! written to a per-content sidecar.
//!
//! The sidecars are CONSUMED by the force-materialize expansion hook
//! ([`crate::pm_engine::phantom_closure`]): it reads them to seed the
//! selective-subtree closure with each flagged importer, so a poisoned version is
//! ejected project-local through #319's graph-aware materialization plan. This
//! module is the PRODUCER (scan + sidecar) half; `phantom_closure` is the
//! consumer half. The two share [`store_v1_dir`]/[`phantom_cache_dir`] so their
//! store handle and sidecar path derive from ONE base and cannot drift apart.
//!
//! With the opt-out set (`NUB_DYNAMIC_PHANTOM_EJECT=0`) this module registers
//! nothing — no extract hook — so the install path is byte-identical to a build
//! without the scanner (a pure-symlink tree).

use std::path::{Path, PathBuf};

use aube_store::{PackageIndex, index_content_fingerprint};
use nub_phantom_scan::scan_index;
use rayon::prelude::*;

/// Whether dynamic phantom detection + ancestor-closure eject is armed — the
/// DEFAULT. Unset (or empty, or any non-falsey value) is ON; only an explicit
/// falsey `NUB_DYNAMIC_PHANTOM_EJECT` (`0`/`false`/`no`/`off`) is the opt-out to
/// a pure-symlink tree. This is the SINGLE arm both halves gate on — the
/// extract-time PRODUCER here and the link-time CONSUMER
/// ([`crate::pm_engine::phantom_closure`]) call this one function, and the
/// install-state fingerprint ([`settings_fingerprint`]) folds THIS value, so
/// detection, closure, and warm-tree invalidation can never drift.
pub(crate) fn enabled() -> bool {
    match std::env::var("NUB_DYNAMIC_PHANTOM_EJECT") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// The effective phantom-eject setting as a stable token, folded into aube's
/// install-state `settings_hash` through the embedder `extra_settings_fingerprint`
/// hook (nub's [`crate::pm_engine::identity::NUB`] profile points that hook here).
/// The flag is nub's, not an aube setting, so it can't ride the resolved-settings
/// hash — this seam is what makes flipping it invalidate the warm tree: nub's
/// default moving across an upgrade, or a user's `NUB_DYNAMIC_PHANTOM_EJECT=0`
/// opt-out, changes this token, so the link phase re-runs and converts the tree
/// to the new materialization shape instead of trusting a stale node_modules.
pub(crate) fn settings_fingerprint() -> String {
    format!("dynamic_phantom_eject={}", enabled())
}

/// Register the extract-time scan hook with the embedded engine. Called once at
/// engine-session build. No-op only under the `NUB_DYNAMIC_PHANTOM_EJECT=0`
/// opt-out, in which case the path pulls in nothing and stays byte-identical.
/// The registration is process-global set-once; a second call is ignored. The
/// link-time consumption of the sidecars this writes lives in
/// [`crate::pm_engine::phantom_closure`], not here.
pub fn register() {
    if !enabled() {
        return;
    }
    let Some(dir) = phantom_cache_dir() else {
        return;
    };
    // Extract-time scan: overlap per-version analysis with the fetch phase.
    aube_store::set_extract_hook(Box::new(move |index: &PackageIndex| {
        scan_and_cache(&dir, index);
    }));
}

/// Scan one freshly-imported package index and persist its verdict to the
/// per-content sidecar. Best-effort throughout — any failure simply leaves no
/// sidecar, which the linker reads as "no eject" (a scan miss must never itself
/// force materialization).
///
/// Panic-safety rests on the scan being panic-free BY CONSTRUCTION, not on the
/// `catch_unwind`: oxc reports an unparseable/hostile file via a return flag (not
/// an unwind), `serde`/`fs` return `Result`, and the graph walk is depth- and
/// size-bounded — so a crafted tarball degrades to a scan miss, never a crash.
/// The `catch_unwind` is a redundant guard that only engages under an unwinding
/// profile (dev/test); the shipped release profile is `panic = "abort"`, where it
/// is inert. Do not treat it as a production safety net.
///
/// The written JSON is the serialized [`nub_phantom_scan::ScanResult`], read back
/// by the CONSUMER ([`crate::pm_engine::phantom_closure`]) — which, being in
/// nub-cli, deserializes it into the typed `ScanResult` (no cross-fork string
/// coupling) to seed the force-materialize closure.
fn scan_and_cache(dir: &Path, index: &PackageIndex) {
    let fingerprint = index_content_fingerprint(index);
    let sidecar = dir.join(format!("{fingerprint}.json"));
    // Cross-process / warm cache hit: this exact content was already scanned.
    // The verdict is a pure function of the immutable bytes, so a concurrent
    // first-writer race is benign (identical result) — skip the redundant scan.
    if sidecar.exists() {
        return;
    }

    // `StoredFile.store_path` is the absolute CAS blob; the scanner resolves the
    // reachable graph over the relpath key set and reads content from the blobs.
    let files: Vec<(String, PathBuf)> = index
        .iter()
        .map(|(rel, file)| (rel.clone(), file.store_path.clone()))
        .collect();
    let Some(result) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan_index(&files)))
            .ok()
            .flatten()
    else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(&result) {
        let _ = std::fs::create_dir_all(dir);
        // Atomic publish: write a per-call-unique temp then rename, so a
        // concurrent installer's linker never observes a half-written sidecar.
        // (The reader already degrades a torn read to "no eject" and self-heals,
        // but rename closes the window.) The temp name carries the pid AND a
        // process-wide sequence: two rayon tasks scanning the SAME content
        // fingerprint — an npm-alias and its real package share one CAS index, so
        // the backfill can enqueue both — must not write the same temp path.
        // Concurrent renames of the same content to the same target are
        // last-writer-wins and byte-identical.
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!("{fingerprint}.{}.{seq}.tmp", std::process::id()));
        if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, &sidecar).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Nub's CAS store schema dir: `<nub-data>/store/v1/`, the parent of the CAS
/// `files/` and `index/` tiers and the `phantom/` sidecar tier. Derives from the
/// SAME [`crate::pm_engine::nub_data_dir`] nub configures its `storeDir` setting
/// from (`nub_data_dir()/store`), plus aube's `v1/` schema suffix — so the store
/// handle and the sidecar dir share ONE base with the real store and cannot drift
/// (the XDG resolution is not re-implemented here). `None` when no data home
/// resolves. `pub(crate)` so the sidecar CONSUMER
/// ([`crate::pm_engine::phantom_closure`]) derives its store handle from the same
/// base this producer uses.
pub(crate) fn store_v1_dir() -> Option<PathBuf> {
    Some(crate::pm_engine::nub_data_dir()?.join("store/v1"))
}

/// The per-content sidecar directory: `<nub-data>/store/v1/phantom/`, next to the
/// CAS + index tiers. `None` when no data home resolves (the scanner then simply
/// doesn't arm). `pub(crate)` so the consumer reads the same directory this
/// producer writes.
pub(crate) fn phantom_cache_dir() -> Option<PathBuf> {
    Some(store_v1_dir()?.join("phantom"))
}

/// Pre-install BACKFILL pass — the warm-store companion to the extract hook,
/// called from `run_engine` (so it runs on `nub install` / `nub ci`, NOT on the
/// `add`/`remove`/`update`/`dedupe` verbs, which dispatch the engine directly).
/// On those verbs a warm-cache package with no sidecar is simply not scanned this
/// run (its eject waits for the next `install`); a fresh FETCH is still covered by
/// the extract hook. A miss only ever means "no eject", never a false break.
///
/// The extract hook fires only on a genuine tarball FETCH; a package already in
/// the CAS with no sidecar (the feature shipping over a warm cache, or a GC'd
/// sidecar) is never re-scanned, so link finds no sidecar and doesn't eject.
/// This pass runs BEFORE `install::run`, so the sidecars exist before link's
/// per-package eject decision on THIS install: for each resolved package already
/// materialized in the CAS but missing a sidecar, it computes and persists the
/// verdict — reusing the same [`scan_and_cache`] the extract hook uses, so the
/// fingerprint keying, sidecar-hit skip, panic-guard, and atomic write are one
/// implementation.
///
/// The `NUB_DYNAMIC_PHANTOM_EJECT=0` opt-out is a STRICT no-op: it returns
/// before any lockfile parse, store access, or fs touch, so that install path is
/// byte-identical. No lockfile / an unparseable one is also a no-op — a fresh
/// install has nothing
/// cached to backfill, and the extract hook covers whatever it fetches. The scan
/// is CPU-bound and per-package independent, so it fans out across rayon; a
/// warm re-install where every sidecar already exists does zero scanning (each
/// package is a `scan_and_cache` sidecar-hit early return).
///
/// Best-effort completeness, never correctness (a miss reads as "no eject"):
/// - Assumes nub's DEFAULT `storeDir` (`store_v1_dir`). A user `store-dir`
///   override (`.npmrc`/yaml) moves the CAS elsewhere, so `load_index` misses
///   and the warm-cache backfill no-ops — fresh fetches still eject via the
///   extract hook; only warm-cache eject is lost under an override.
/// - Keys `load_index` on `pkg.integrity`; no-integrity packages (git deps /
///   integrity-stripped proxies) that the linker reads via a computed-sha
///   binding may not be backfilled. No false eject — the linker recomputes the
///   fingerprint from its own index, so an absent/mismatched sidecar is "no eject".
pub fn backfill_from_lockfile(project_dir: &Path) {
    if !enabled() {
        return;
    }
    let (Some(sidecar_dir), Some(store_v1)) = (phantom_cache_dir(), store_v1_dir()) else {
        return;
    };
    let Ok(manifest) = aube_manifest::PackageJson::from_path(&project_dir.join("package.json"))
    else {
        return;
    };
    let Ok(graph) = aube_lockfile::parse_lockfile(project_dir, &manifest) else {
        return;
    };
    // `Store::at` takes the CAS `files/` root; `store_v1_dir` is its parent, and
    // the store derives `index/` from it — matching how nub's configured
    // `storeDir` resolves to `<storeDir>/v1/files` (aube `open_store`).
    let store = aube_store::Store::at(store_v1.join("files"));
    // BTreeMap has no rayon bridge; collect the resolved set first.
    let packages: Vec<&aube_lockfile::LockedPackage> = graph.packages.values().collect();
    packages.into_par_iter().for_each(|pkg| {
        // Not in the CAS ⇒ skip: the resolve/fetch phase fetches it and the
        // extract hook covers it. `registry_name()` + `integrity` key the index
        // the SAME way the linker's own eject-gate read does (`builder.rs`
        // `force_materialize` path), so npm-alias deps resolve to the right blob.
        let Some(index) =
            store.load_index(pkg.registry_name(), &pkg.version, pkg.integrity.as_deref())
        else {
            return;
        };
        scan_and_cache(&sidecar_dir, &index);
    });
}
