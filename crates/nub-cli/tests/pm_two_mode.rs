//! The two-mode model, behaviorally, through the binary: `pm use nub`'s
//! offline migration invariants, the role-first lifecycle UA, and the
//! nub-identity config gating (stray-yaml warning). All rows run OFFLINE —
//! `pm use nub` never touches a registry by design, the install rows use
//! empty-dependency manifests, and every project points its registry at a
//! dead port so accidental network fails loudly. The online halves (real
//! pnpm judging the reversed state) live in tests/aube-conformance (the
//! `nub` format leg) and tests/brand-sweep.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("nub");
    path
}

fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nub-two-mode-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".npmrc"), "registry=http://127.0.0.1:1/\n").unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    dir
}

fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(nub_binary())
        .args(args)
        .current_dir(dir)
        .env("XDG_DATA_HOME", dir.join("xdg-data"))
        .env("XDG_CACHE_HOME", dir.join("xdg-cache"))
        .output()
        .expect("failed to spawn nub");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

const EMPTY_LOCK: &str = "lockfileVersion: '9.0'\n\nimporters:\n\n  .: {}\n";

/// `pm use nub` on a single-package pnpm project carrying a catalog: the
/// whole switch is offline, the yaml dies, the catalog lands as a
/// packages-less `workspaces` object (the Bun shape), settings land in
/// `.npmrc`, and the lockfile is renamed byte-identically. Rerunning is a
/// no-op (idempotence is the contract).
#[test]
fn use_nub_migrates_a_single_package_catalog_project_offline_and_idempotently() {
    let dir = project(
        "use-nub",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"pnpm@10.0.0"}"#,
            ),
            ("pnpm-lock.yaml", EMPTY_LOCK),
            (
                "pnpm-workspace.yaml",
                "catalog:\n  left-pad: 1.3.0\nminimumReleaseAge: 1440\nproduction: true\n",
            ),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["pm", "use", "nub"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    // Zero pnpm-named files; lock.yaml carries the exact prior bytes.
    assert!(!dir.join("pnpm-workspace.yaml").exists(), "{stdout}");
    assert!(!dir.join("pnpm-lock.yaml").exists(), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.join("lock.yaml")).unwrap(),
        EMPTY_LOCK,
        "the rename must be byte-identical"
    );

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["packageManager"],
        format!("nub@{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(manifest["devEngines"]["packageManager"]["name"], "nub");
    assert_eq!(manifest["devEngines"]["packageManager"]["onFail"], "warn");
    assert_eq!(
        manifest["workspaces"]["catalog"]["left-pad"], "1.3.0",
        "single-package catalogs land as a packages-less workspaces object"
    );
    assert!(
        manifest["workspaces"].get("packages").is_none(),
        "no packages key must be invented for a single-package repo"
    );

    let npmrc = std::fs::read_to_string(dir.join(".npmrc")).unwrap();
    assert!(
        npmrc.contains("minimum-release-age=1440"),
        "settings must land in .npmrc: {npmrc}"
    );
    assert!(
        stdout.contains("production"),
        "the warn tail must name the transient key loudly: {stdout}"
    );
    assert!(
        stdout.contains("corepack") && stdout.contains("nub pm use pnpm"),
        "the summary must carry the teammates consequences block: {stdout}"
    );

    // Idempotent rerun: same identity, lockfile kept, nothing new to migrate.
    let (stdout2, stderr2, code2) = run(&dir, &["pm", "use", "nub"]);
    assert_eq!(code2, 0, "stdout: {stdout2}\nstderr: {stderr2}");
    assert!(
        stdout2.contains("lock.yaml: kept"),
        "rerun must keep, not re-convert: {stdout2}"
    );
}

/// Crash-recovery for the one half-migrated window `use nub` can be killed in:
/// the manifest edit (atomic — packageManager flipped, the yaml's catalog
/// already copied into `workspaces`, the `pnpm` namespace dropped) and the
/// lockfile rename both completed, but the process died BEFORE the final
/// `pnpm-workspace.yaml` deletion. The project then declares nub identity yet
/// still carries the stray yaml. The recovery contract is re-run idempotence,
/// not atomicity: a second `use nub` reads the leftover yaml, re-derives the
/// (already-present) migration, deletes the yaml, and lands in clean nub
/// identity with the catalog data intact — never silently dropped. We build
/// the half-state directly rather than racing a real SIGKILL (the command is
/// too fast to interrupt mid-write reliably).
#[test]
fn use_nub_recovers_from_a_crash_before_the_yaml_deletion() {
    let nub_ver = env!("CARGO_PKG_VERSION");
    let dir = project(
        "use-nub-halfstate",
        &[
            // Manifest as the atomic edit would have left it: nub identity,
            // catalog migrated into the workspaces object, no pnpm namespace.
            (
                "package.json",
                &format!(
                    r#"{{"name":"app","version":"1.0.0","packageManager":"nub@{nub_ver}",{}}}"#,
                    r#""devEngines":{"packageManager":{"name":"nub","version":"^0.0.0","onFail":"warn"}},"workspaces":{"catalog":{"left-pad":"1.3.0"}}"#
                ),
            ),
            // The rename already happened: lock.yaml present, no pnpm-lock.yaml.
            ("lock.yaml", EMPTY_LOCK),
            // The leftover the crash never got to delete — still carrying the
            // catalog that the atomic manifest edit already preserved.
            ("pnpm-workspace.yaml", "catalog:\n  left-pad: 1.3.0\n"),
        ],
    );

    let (stdout, stderr, code) = run(&dir, &["pm", "use", "nub"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    // The leftover yaml is gone; lock.yaml is kept (not re-renamed); the
    // project is now in clean, fully-migrated nub identity.
    assert!(
        !dir.join("pnpm-workspace.yaml").exists(),
        "the recovery run must delete the stray yaml: {stdout}"
    );
    assert!(
        dir.join("lock.yaml").is_file() && !dir.join("pnpm-lock.yaml").exists(),
        "lock.yaml stays the lockfile, untouched: {stdout}"
    );

    // The migrated data survived the half-state — never silently dropped.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(manifest["packageManager"], format!("nub@{nub_ver}"));
    assert_eq!(
        manifest["workspaces"]["catalog"]["left-pad"], "1.3.0",
        "the catalog must remain in the manifest after recovery"
    );
    assert!(
        manifest.get("pnpm").is_none(),
        "the pnpm namespace stays removed"
    );
}

/// Injected-deps state refuses the whole switch before anything is written —
/// the engine has no injected-deps implementation, and `use` never silently
/// changes install semantics.
#[test]
fn use_nub_refuses_injected_deps_with_nothing_written() {
    let dir = project(
        "injected",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"pnpm@10.0.0","dependenciesMeta":{"sibling":{"injected":true}}}"#,
            ),
            ("pnpm-lock.yaml", EMPTY_LOCK),
            ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["pm", "use", "nub"]);
    assert_ne!(code, 0, "injected deps must refuse: {stdout}");
    assert!(
        stderr.contains("injected") && stderr.contains("package.json"),
        "the refusal must name the state and where it lives: {stderr}"
    );
    assert!(
        dir.join("pnpm-workspace.yaml").exists()
            && dir.join("pnpm-lock.yaml").exists()
            && !dir.join("lock.yaml").exists(),
        "a refusal must leave the project byte-untouched"
    );
    let manifest = std::fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(
        manifest.contains("pnpm@10.0.0"),
        "the declaration must be untouched after the refusal"
    );
}

/// Under nub identity a stray pnpm-workspace.yaml is ignore-with-warning:
/// exactly one warning naming it unread plus the remedies, and the install
/// proceeds against lock.yaml.
#[test]
fn stray_workspace_yaml_under_nub_identity_warns_once_and_install_proceeds() {
    let dir = project(
        "stray-yaml",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"nub@0.0.1"}"#,
            ),
            ("lock.yaml", EMPTY_LOCK),
            ("pnpm-workspace.yaml", "nodeLinker: hoisted\n"),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["install"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stderr
            .matches("pnpm-workspace.yaml is not read under nub identity")
            .count(),
        1,
        "exactly one warning: {stderr}"
    );
    assert!(
        stderr.contains("nub pm use pnpm") && stderr.contains("nub pm use nub"),
        "the warning must carry both remedies: {stderr}"
    );
    assert!(
        dir.join("lock.yaml").is_file() && !dir.join("pnpm-lock.yaml").exists(),
        "lock.yaml stays the lockfile"
    );
}

/// The role-first lifecycle UA, observed by a real root postinstall: a
/// pnpm-declared project is served pnpm-first at the PINNED version with the
/// nub token second; a nub-identity project is nub-first in the runner
/// dialect. (The fresh + engine-parity cases live in tests/brand-sweep and
/// the pm_engine unit tests.)
#[test]
fn lifecycle_ua_is_pnpm_first_in_compat_and_nub_first_under_nub_identity() {
    let postinstall = r#""scripts":{"postinstall":"node -e \"require('fs').writeFileSync('ua.txt', process.env.npm_config_user_agent||'')\""}"#;

    let dir = project(
        "ua-compat",
        &[(
            "package.json",
            &format!(
                r#"{{"name":"app","version":"1.0.0","packageManager":"pnpm@9.9.9",{postinstall}}}"#
            ),
        )],
    );
    let (stdout, stderr, code) = run(&dir, &["install"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let ua = std::fs::read_to_string(dir.join("ua.txt")).expect("postinstall must run");
    assert!(
        ua.starts_with(&format!(
            "pnpm/9.9.9 nub/{} node/v",
            env!("CARGO_PKG_VERSION")
        )),
        "compat UA must be pnpm-first at the pinned version, nub second: {ua}"
    );

    let dir = project(
        "ua-nub",
        &[
            (
                "package.json",
                &format!(
                    r#"{{"name":"app","version":"1.0.0","packageManager":"nub@0.0.1",{postinstall}}}"#
                ),
            ),
            ("lock.yaml", EMPTY_LOCK),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["install"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let ua = std::fs::read_to_string(dir.join("ua.txt")).expect("postinstall must run");
    assert!(
        ua.starts_with(&format!("nub/{} npm/? node/v", env!("CARGO_PKG_VERSION"))),
        "nub-identity UA must be nub-first in the runner dialect: {ua}"
    );
}

/// An empty-dependency, in-sync npm v3 package-lock — converts to lock.yaml
/// offline (no graph to fetch), exercising the npm→nub `Convert` path.
const EMPTY_NPM_LOCK: &str = r#"{
  "name": "app",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": { "": { "name": "app", "version": "1.0.0" } }
}
"#;

/// The phantom-dependency layout-change warning (writeup §6): switching a
/// project FROM a hoisting PM (npm/yarn — flat node_modules) to nub's isolated
/// layout can break undeclared imports, so `pm use nub` warns. The warning is
/// gated to npm/yarn only — pnpm/bun are already isolated, and a fresh project
/// has no incumbent layout to change. stderr is a pipe here, so the text is
/// plain (no ANSI), matching a NO_COLOR / non-terminal shell.
#[test]
fn use_nub_warns_about_phantom_deps_only_when_leaving_a_hoisting_pm() {
    let pkg = r#"{"name":"app","version":"1.0.0"}"#;

    // npm incumbent → the layout-change warning fires.
    let npm = project(
        "phantom-npm",
        &[("package.json", pkg), ("package-lock.json", EMPTY_NPM_LOCK)],
    );
    let (stdout, stderr, code) = run(&npm, &["pm", "use", "nub"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("isolated node_modules")
            && stderr.contains("phantom dependencies")
            && stderr.contains("npm and yarn"),
        "npm→nub must warn that the isolated layout can break phantom deps: {stderr}"
    );

    // pnpm incumbent → already isolated, no phantom warning.
    let pnpm = project(
        "phantom-pnpm",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"pnpm@10.0.0"}"#,
            ),
            ("pnpm-lock.yaml", EMPTY_LOCK),
        ],
    );
    let (stdout, stderr, code) = run(&pnpm, &["pm", "use", "nub"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("phantom dependencies"),
        "pnpm is already non-hoisting — no phantom-deps warning: {stderr}"
    );

    // Fresh project (no lockfile) → no incumbent layout, no warning.
    let fresh = project("phantom-fresh", &[("package.json", pkg)]);
    let (stdout, stderr, code) = run(&fresh, &["pm", "use", "nub"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("phantom dependencies"),
        "a fresh project has no incumbent layout — no phantom-deps warning: {stderr}"
    );
}

/// A `.pnpmfile.cjs` is pnpm-proprietary AND shapes resolution (its
/// hooks rewrite the dep graph), so under a non-pnpm incumbent it's
/// another tool's config and must not be honored. A `preResolution` hook
/// writes a marker file when it runs — the cleanest cross-tool "did the
/// hook fire?" signal. `--no-frozen-lockfile` forces the resolve so the
/// hook actually gets a chance to run (a frozen/already-current install
/// short-circuits before pnpmfile detection).
///
/// npm incumbent: the cwd-default `.pnpmfile` is gated off — the hook
/// never runs and exactly one dim warning names the file + the incumbent.
#[test]
fn pnpmfile_ignored_under_npm_incumbent_with_one_warning() {
    let hook = r#"module.exports = { hooks: { preResolution(ctx) { require('fs').writeFileSync('hook-ran.txt', 'yes'); return ctx; } } };"#;
    let dir = project(
        "pnpmfile-npm",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"npm@10.0.0"}"#,
            ),
            (
                "package-lock.json",
                r#"{"name":"app","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"app","version":"1.0.0"}}}"#,
            ),
            (".pnpmfile.cjs", hook),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["install", "--no-frozen-lockfile"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !dir.join("hook-ran.txt").exists(),
        "the cwd-default .pnpmfile must NOT run under an npm incumbent: {stderr}"
    );
    assert_eq!(
        stderr.matches(".pnpmfile.cjs` ignored").count(),
        1,
        "exactly one ignore warning naming the file: {stderr}"
    );
    assert!(
        stderr.contains("this project uses npm")
            && stderr.contains("--pnpmfile")
            && stderr.contains("nub pm use pnpm"),
        "the warning names the incumbent and both escape hatches: {stderr}"
    );
}

/// pnpm incumbent: the cwd-default `.pnpmfile` is honored exactly as
/// upstream — the hook runs and there is no ignore warning. This is the
/// pnpm "special relationship": its proprietary config stays live when
/// pnpm is the incumbent.
#[test]
fn pnpmfile_honored_under_pnpm_incumbent_without_warning() {
    let hook = r#"module.exports = { hooks: { preResolution(ctx) { require('fs').writeFileSync('hook-ran.txt', 'yes'); return ctx; } } };"#;
    let dir = project(
        "pnpmfile-pnpm",
        &[
            (
                "package.json",
                r#"{"name":"app","version":"1.0.0","packageManager":"pnpm@9.9.9"}"#,
            ),
            ("pnpm-lock.yaml", EMPTY_LOCK),
            (".pnpmfile.cjs", hook),
        ],
    );
    let (stdout, stderr, code) = run(&dir, &["install", "--no-frozen-lockfile"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        dir.join("hook-ran.txt").is_file(),
        "the cwd-default .pnpmfile must run under a pnpm incumbent: {stderr}"
    );
    assert_eq!(
        stderr.matches("ignored").count(),
        0,
        "no ignore warning when pnpm is the incumbent: {stderr}"
    );
}
