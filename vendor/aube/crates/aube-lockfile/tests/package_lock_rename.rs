//! Regression cover for an embedder whose canonical lockfile basename does
//! NOT share pnpm's `.yaml` extension (e.g. `package.lock`), plus a legacy
//! basename honored on read during a rename transition.
//!
//! Own integration-test binary (= own process) because the active embedder is
//! once-per-process.

use aube_lockfile::{
    LockfileKind, aube_lock_filename, detect_existing_lockfile_kind, pnpm_lock_filename,
    write_lockfile_as,
};
use aube_util::Embedder;

static MYTOOL: Embedder = Embedder {
    name: "mytool",
    display_name: "mytool",
    vendor: None,
    version: "1.0.0",
    user_agent: "mytool/1.0.0",
    self_names: &["mytool"],
    compatible_names: &["pnpm"],
    // The renamed canonical: a `.lock` extension, NOT `.yaml`.
    lockfile_basename: "package.lock",
    // The pre-rename name, recognized on read but never written.
    lockfile_legacy_basenames: &["lock.yaml"],
    workspace_yaml: None,
    manifest_namespace: "",
    env_prefix: None,
    config_env_prefix: Some("MYTOOL"),
    diag_env_prefix: Some("MYTOOL"),
    cache_namespace: "mytool",
    data_namespace: "mytool",
    managed_config_system_dir: Some("mytool"),
    config_namespace: None,
    // Mirror nub: the canonical does not silently outrank a foreign lockfile.
    canonical_lockfile_always_wins: false,
    runtime_switching: false,
    self_engines_check: false,
    self_update_enabled: false,
    warm_store_verify: false,
    no_churn_lockfile_write: true,
    read_branded_settings_env: false,
    gvs_incompatible_warning: false,
    gvs_over_default_hoist: false,
    primer_ttl: None,
    cpu_budget: None,
    tty_progress: false,
    strict_unsupported_source: true,
    warm_trust_revalidate: false,
    trust_policy_ignore_after_default: None,
    extra_settings_fingerprint: None,
};

#[test]
fn non_yaml_basename_keeps_pnpm_name_and_reads_both_names() {
    aube_util::set_embedder(&MYTOOL);

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"t"}"#).unwrap();

    // The canonical write name follows the basename...
    assert_eq!(LockfileKind::Aube.filename(), "package.lock");
    assert_eq!(aube_lock_filename(dir.path()), "package.lock");
    // ...but the pnpm name must stay `pnpm-lock.yaml`, NOT be derived from the
    // canonical basename's `.lock` extension (the regression this guards): a
    // pnpm-incumbent project must keep writing pnpm's real file.
    assert_eq!(pnpm_lock_filename(dir.path()), "pnpm-lock.yaml");

    // A fresh Aube write lands at the renamed canonical.
    let graph = aube_lockfile::LockfileGraph::default();
    let manifest = aube_manifest::PackageJson::default();
    let written = write_lockfile_as(dir.path(), &graph, &manifest, LockfileKind::Aube).unwrap();
    assert_eq!(written, dir.path().join("package.lock"));

    // The corruption this pins: a pnpm-incumbent project written under the
    // renamed profile must land at `pnpm-lock.yaml`, NEVER `pnpm-lock.lock`
    // (which the old "reuse the canonical basename's extension" derivation
    // would have produced once the canonical extension became `.lock`).
    let pnpm_dir = tempfile::tempdir().unwrap();
    std::fs::write(pnpm_dir.path().join("package.json"), r#"{"name":"t"}"#).unwrap();
    let pnpm_written =
        write_lockfile_as(pnpm_dir.path(), &graph, &manifest, LockfileKind::Pnpm).unwrap();
    assert_eq!(pnpm_written, pnpm_dir.path().join("pnpm-lock.yaml"));
    assert!(
        !pnpm_dir.path().join("pnpm-lock.lock").exists(),
        "no `.lock`-extension pnpm lockfile may ever be produced"
    );

    // Read-both: a project still carrying only the LEGACY name resolves as the
    // canonical (Aube) kind so every read path keeps working pre-migration.
    let legacy = tempfile::tempdir().unwrap();
    std::fs::write(legacy.path().join("package.json"), r#"{"name":"t"}"#).unwrap();
    std::fs::write(legacy.path().join("lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    assert_eq!(
        detect_existing_lockfile_kind(legacy.path()),
        Some(LockfileKind::Aube),
        "a legacy lock.yaml must still resolve as the canonical kind"
    );

    // When both names exist, the current name wins (legacy trails it in the
    // candidate order).
    std::fs::write(
        legacy.path().join("package.lock"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    assert_eq!(
        detect_existing_lockfile_kind(legacy.path()),
        Some(LockfileKind::Aube)
    );
}
