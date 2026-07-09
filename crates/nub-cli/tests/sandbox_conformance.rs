//! Cross-platform sandbox CONFORMANCE matrix — the done-gate driver.
//!
//! Drives the REAL `nub run --sandbox <policy.json> -- <probe> <action> <target>`
//! CLI seam (design.md §2.6) against a committed, declarative fixture matrix
//! (`tests/sandbox_conformance/*.json`). Each fixture pairs a surface `sandbox`
//! policy with per-axis probe cases + an expected allow/deny verdict; every DENY
//! case is RE-RUN under the fixture's `relaxed` policy as a NEGATIVE CONTROL, so a
//! hollow / no-op enforcement (which would let the denied action through) cannot
//! pass — the relaxed run must succeed or the deny is proven meaningless.
//!
//! Scope split (deliberate): fs (read/write incl. the secret deny-set + `.env`
//! subtree) and env (scrub incl. the npm_config auth-not-leaked case) are proven
//! black-box on ALL THREE OSes here. The deeper per-host proxy filtering and the
//! Linux seccomp syscall denials (socket/ptrace/vmread) live in the per-backend
//! enforcement tests (`{macos,linux,windows}_enforcement`, `{macos,linux}_proxy`)
//! that run alongside this in the conformance CI workflow — the probe is black-box
//! and cannot introspect a proxy's SNI decision or a raw syscall the way those do.
//! net here is the coarse egress-deny axis, which IS uniform across the backends.
//!
//! Hermetic: every case gets its own `TempDir` tree (project + a fake home holding
//! the secrets + an outside dir), so the suite is order- and thread-independent and
//! the secret denies target fixture paths, never the developer's real `~`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

// ── fixture format ──────────────────────────────────────────────────────────────

/// One conformance fixture: a policy, its negative-control relaxation, an optional
/// ambient-env injection (env fixtures), and the probe cases.
#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    /// The surface `sandbox` block under test (compiled by `nub run --sandbox`).
    policy: serde_json::Value,
    /// The relaxation that must ADMIT every deny case — the negative control.
    relaxed: serde_json::Value,
    /// Extra env layered onto nub's ambient before spawn (env fixtures inject the
    /// secrets + build hints the scrub is asserted against). `$HOME` expands to the
    /// fixture home; PATH is always seeded so nub and the probe run.
    #[serde(default)]
    ambient_env: BTreeMap<String, String>,
    cases: Vec<Case>,
}

/// One probe case: attempt `probe` against `target`, expect `expect` (allowed) under
/// the fixture policy. `platforms` restricts the case to the named OSes (default all).
#[derive(Debug, Deserialize)]
struct Case {
    /// `read` | `write` | `connect` | `env`.
    probe: String,
    /// Path spec (`proj:rel`, `home:rel`, `out:rel`), `host:port`, or an env key.
    target: String,
    /// Whether the action is expected to SUCCEED (be allowed) under `policy`.
    expect: bool,
    /// OSes this case applies to (`macos`/`linux`/`windows`); empty = all.
    #[serde(default)]
    platforms: Vec<String>,
}

// ── binary locations ──────────────────────────────────────────────────────────────

/// A sibling binary in the same target dir as this test's deps/ (cargo builds all of
/// nub-cli's bins — `nub` and `nub-sandbox-probe` — before its integration tests).
fn sibling_bin(name: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // deps/
    p.pop(); // debug/ (or fast/)
    p.push(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(p.exists(), "expected built binary at {}", p.display());
    p
}

fn nub_bin() -> PathBuf {
    sibling_bin("nub")
}
fn probe_bin() -> PathBuf {
    sibling_bin("nub-sandbox-probe")
}

// ── Landlock gate (Linux only) ────────────────────────────────────────────────────

/// Linux fs/net enforcement rides Landlock+seccomp; on a kernel without Landlock the
/// deny cases would silently "pass" (nothing enforced), so SKIP them there — unless
/// `NUB_SANDBOX_REQUIRE_LANDLOCK=1` (the conformance-CI real-kernel leg), where a
/// missing Landlock must fail loudly rather than read as green. env fixtures are
/// parent-side construction (kernel-independent) and never gated.
#[cfg(target_os = "linux")]
fn linux_enforceable() -> bool {
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u64,
        )
    };
    if abi >= 2 {
        return true;
    }
    let required = matches!(
        std::env::var("NUB_SANDBOX_REQUIRE_LANDLOCK").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    assert!(
        !required,
        "NUB_SANDBOX_REQUIRE_LANDLOCK set but no Landlock ABI>=2 — fs/net conformance \
         cannot be proven on this kernel (real-kernel gate)"
    );
    false
}

// ── the hermetic fixture tree ─────────────────────────────────────────────────────

/// A throwaway project + fake home + outside dir. `resolve` turns a `proj:`/`home:`/
/// `out:` target spec into a concrete path and materializes it (a read target as a
/// real file, a write target's parent dir) so a denial is a genuine deny, never a
/// missing-file artifact.
struct Tree {
    _tmp: TempDir,
    proj: PathBuf,
    home: PathBuf,
    outside: PathBuf,
}

impl Tree {
    fn new() -> Self {
        // /private/tmp on macOS avoids $TMPDIR (= the DARWIN confstr scratch the
        // backend always write-grants, which would make every write spuriously pass).
        let builder = tempfile::Builder::new().prefix("nub-conf-").to_owned();
        let tmp = if cfg!(target_os = "macos") {
            builder.tempdir_in("/private/tmp")
        } else {
            builder.tempdir()
        }
        .expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let proj = root.join("proj");
        let home = root.join("home");
        let outside = root.join("outside");
        for d in [&proj, &home, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        Tree {
            _tmp: tmp,
            proj,
            home,
            outside,
        }
    }

    /// Resolve a `proj:`/`home:`/`out:` spec (fs probes) to a concrete path and
    /// materialize it. Non-fs specs are returned unchanged (host:port, env key).
    fn resolve(&self, probe: &str, target: &str) -> String {
        if probe != "read" && probe != "write" {
            return target.to_string();
        }
        let (anchor, rel) = target.split_once(':').unwrap_or(("proj", target));
        let base = match anchor {
            "proj" => &self.proj,
            "home" => &self.home,
            "out" => &self.outside,
            other => panic!("unknown path anchor {other:?} in {target:?}"),
        };
        let path = base.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // A read target must EXIST so a deny is a real deny; a write target must not
        // (write creates it) but its parent (made above) must.
        if probe == "read" {
            std::fs::write(&path, b"CONFORMANCE-SECRET").unwrap();
        }
        path.to_string_lossy().into_owned()
    }
}

// ── the driver ────────────────────────────────────────────────────────────────────

fn load_fixture(name: &str) -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sandbox_conformance")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing fixture {name}: {e}"))
}

fn applies_here(case: &Case) -> bool {
    case.platforms.is_empty() || case.platforms.iter().any(|p| p == std::env::consts::OS)
}

/// Run one probe under `policy` in `tree`; true = the action was ALLOWED (nub, and
/// hence the probe, exited 0). A nub-side compile/apply failure is a HARNESS error
/// (never a silent "deny"), surfaced loudly with nub's stderr.
fn run_case(
    tree: &Tree,
    policy: &serde_json::Value,
    ambient: &BTreeMap<String, String>,
    probe: &str,
    target: &str,
) -> bool {
    let policy_path = tree.proj.join("__policy.json");
    std::fs::write(&policy_path, serde_json::to_string(policy).unwrap()).unwrap();

    let mut cmd = Command::new(nub_bin());
    cmd.arg("run").arg("--sandbox").arg(&policy_path).arg("--");
    cmd.arg(probe_bin()).arg(probe);
    // `connect` takes host + port as two probe args; everything else is one.
    if probe == "connect" {
        let (host, port) = target.split_once(':').expect("connect target host:port");
        cmd.arg(host).arg(port);
    } else {
        cmd.arg(target);
    }
    cmd.current_dir(&tree.proj);
    // Homes come from the ambient env inside nub (`sandbox_homes`), so point them at
    // the fixture home → the `...` secret denies target fixture paths, not real `~`.
    cmd.env("HOME", &tree.home)
        .env("USERPROFILE", &tree.home)
        .env("XDG_CACHE_HOME", tree.home.join(".cache"));
    for (k, v) in ambient {
        // The registry-scoped `npm_config_//host/:_auth…` key holds a `/`, which the
        // Windows environment block does not represent; its assertion case is already
        // platform-gated to unix, so skip injecting it on Windows rather than risk
        // disturbing the child env block.
        if cfg!(windows) && k.contains('/') {
            continue;
        }
        let v = v.replace("$HOME", &tree.home.to_string_lossy());
        cmd.env(k, v);
    }

    let out = cmd.output().expect("spawn nub");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !(stderr.contains("did not compile") || stderr.contains("could not be applied")),
        "HARNESS ERROR — nub failed to set up the sandbox, not a probe verdict:\n{stderr}"
    );
    out.status.code() == Some(0)
}

/// Drive one fixture: assert every case's verdict, and re-run each DENY case under
/// the relaxed policy as its negative control.
fn drive(name: &str) {
    #[cfg(target_os = "linux")]
    let fs_net_enforceable = linux_enforceable();
    #[cfg(not(target_os = "linux"))]
    let fs_net_enforceable = true;

    let fx = load_fixture(name);
    assert_eq!(fx.name, name, "fixture name mismatch in {name}.json");

    for case in &fx.cases {
        if !applies_here(case) {
            continue;
        }
        // Skip fs/net enforcement assertions on a Landlock-less Linux (env is fine).
        if !fs_net_enforceable && case.probe != "env" {
            continue;
        }

        let tree = Tree::new();
        let target = tree.resolve(&case.probe, &case.target);

        let allowed = run_case(&tree, &fx.policy, &fx.ambient_env, &case.probe, &target);
        assert_eq!(
            allowed, case.expect,
            "[{name}] {} {}: expected allowed={}, got allowed={}",
            case.probe, case.target, case.expect, allowed
        );

        if !case.expect {
            // Negative control: the SAME action under the relaxed policy MUST be
            // allowed — otherwise the deny above proves nothing (missing file,
            // unreachable host, a probe that can't even start).
            let nc = run_case(&tree, &fx.relaxed, &fx.ambient_env, &case.probe, &target);
            assert!(
                nc,
                "[{name}] neg-control {} {}: must be ALLOWED under the relaxed policy \
                 (the deny is hollow otherwise)",
                case.probe, case.target
            );
        }
    }
}

// ── the matrix — one test per fixture (parallel, granular failure attribution) ────

#[test]
fn fs_read_confine() {
    drive("fs-read-confine");
}

#[test]
fn fs_generous_read_secret_denyset() {
    drive("fs-generous-read-secrets");
}

#[test]
fn fs_write_confine() {
    drive("fs-write-confine");
}

#[test]
fn env_scrub_including_npm_config_auth() {
    drive("env-scrub-auth");
}

#[test]
fn net_coarse_egress_deny() {
    drive("net-coarse-deny");
}
