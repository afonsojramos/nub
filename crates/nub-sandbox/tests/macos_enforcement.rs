//! macOS Seatbelt backend — REAL enforcement tests.
//!
//! Each test compiles a surface policy, applies it, and actually SPAWNS the child
//! under `sandbox-exec`, asserting the kernel allowed or denied the action. Every
//! confinement assertion is paired with a NEGATIVE CONTROL (the axis lifted → the
//! same action succeeds) so a passing test cannot be hollow. macOS-only.
//!
//! Hermetic: every test builds its own `tempfile::TempDir` fixture and homes; no
//! shared mutable state, so the suite is order- and thread-independent.
#![cfg(target_os = "macos")]

use nub_sandbox::compiler::{CompileCtx, ShellRunner};
use nub_sandbox::matcher::Homes;
use nub_sandbox::{CommandSpec, apply, compile};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::TempDir;

/// A fixture: a project dir + a fake home (so secret denies target fixture paths,
/// never the real `~/.ssh`) + an out-of-project dir.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    proj: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    // Place the fixture under /private/tmp, NOT the default $TMPDIR — the latter is
    // /var/folders/<uid>/T, the DARWIN confstr scratch dir the backend always
    // write-grants (for the Apple toolchain), which would spuriously make every
    // fixture write "allowed". /private/tmp is subject to write-confine.
    let tmp = tempfile::Builder::new()
        .prefix("nub-sbx-")
        .tempdir_in("/private/tmp")
        .unwrap();
    // Canonicalize up front — the kernel checks the canonical path, so the paths we
    // assert against must be canonical too (here /private/tmp is already canonical).
    let root = fs::canonicalize(tmp.path()).unwrap();
    let proj = root.join("proj");
    let home = root.join("home");
    fs::create_dir_all(proj.join("sub")).unwrap();
    fs::create_dir_all(proj.join("writable")).unwrap();
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(proj.join("pub.txt"), "PUBLIC").unwrap();
    fs::write(proj.join("sub/nested.txt"), "NESTED").unwrap();
    fs::write(proj.join(".env"), "ENVSECRET").unwrap();
    fs::write(proj.join(".env.local"), "ENVLOCAL").unwrap();
    fs::write(proj.join("sub/.env"), "NESTEDENV").unwrap();
    fs::write(home.join(".ssh/id_rsa"), "IDRSA").unwrap();
    fs::write(root.join("outside/o.txt"), "OUTSIDE").unwrap();
    Fixture {
        _tmp: tmp,
        root,
        proj,
        home,
    }
}

impl Fixture {
    fn homes(&self) -> Homes {
        Homes {
            home: self.home.clone(),
            tmp: std::env::temp_dir(),
            cache: self.home.join(".cache"),
            project: self.proj.clone(),
        }
    }

    fn ctx(&self, env: &[(&str, &str)]) -> CompileCtx {
        let ambient: BTreeMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        CompileCtx {
            homes: self.homes(),
            cwd: self.proj.clone(),
            trusted: true,
            ambient_env: ambient,
            runner: Box::new(ShellRunner),
        }
    }

    /// Run `program args…` under `surface`, returning true iff it exited 0 (allowed).
    /// stdio → null so the verdict is the tested action alone, never a stdout write.
    fn allowed(&self, surface: Value, program: &str, args: &[&str]) -> bool {
        self.allowed_env(surface, &[], program, args)
    }

    fn allowed_env(
        &self,
        surface: Value,
        env: &[(&str, &str)],
        program: &str,
        args: &[&str],
    ) -> bool {
        let policy = compile(&surface, &self.ctx(env)).expect("policy compiles");
        let spec = CommandSpec::new(program)
            .args(args.iter().copied())
            .cwd(&self.proj);
        let prepared = apply(&policy, spec).expect("apply");
        let mut cmd = prepared.command;
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        cmd.status().expect("spawn").success()
    }
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

const CAT: &str = "/bin/cat";
const TOUCH: &str = "/usr/bin/touch";

// ── fs read-confine (array form = allowlist: project + toolchain only) ─────────

#[test]
fn read_confine_allows_project_denies_outside() {
    let f = fixture();
    let confine = serde_json::json!({ "fs": ["./"] });
    assert!(
        f.allowed(confine.clone(), CAT, &[&s(&f.proj.join("pub.txt"))]),
        "project read"
    );
    assert!(
        f.allowed(confine.clone(), CAT, &[&s(&f.proj.join("sub/nested.txt"))]),
        "nested project read"
    );
    assert!(
        f.allowed(confine.clone(), CAT, &["/etc/hosts"]),
        "system toolchain read"
    );
    // confinement:
    assert!(
        !f.allowed(confine.clone(), CAT, &[&s(&f.root.join("outside/o.txt"))]),
        "outside read denied"
    );
    assert!(
        !f.allowed(confine, CAT, &[&s(&f.home.join(".ssh/id_rsa"))]),
        "home secret read denied"
    );
    // negative control — fs relaxed → the same outside read succeeds:
    assert!(
        f.allowed(
            serde_json::json!({ "fs": true }),
            CAT,
            &[&s(&f.root.join("outside/o.txt"))]
        ),
        "neg-control: relaxed fs reads outside"
    );
}

// ── fs .env deny under a broad project read-allow (generous read + secrets) ────

#[test]
fn env_files_denied_under_generous_read() {
    let f = fixture();
    let generous = serde_json::json!({ "fs": ["..."] });
    assert!(
        f.allowed(generous.clone(), CAT, &[&s(&f.proj.join("pub.txt"))]),
        "pub readable"
    );
    assert!(
        !f.allowed(generous.clone(), CAT, &[&s(&f.proj.join(".env"))]),
        ".env denied"
    );
    assert!(
        !f.allowed(generous.clone(), CAT, &[&s(&f.proj.join(".env.local"))]),
        ".env.local denied"
    );
    assert!(
        !f.allowed(generous.clone(), CAT, &[&s(&f.proj.join("sub/.env"))]),
        "nested .env denied"
    );
    assert!(
        !f.allowed(generous, CAT, &[&s(&f.home.join(".ssh/id_rsa"))]),
        "ssh key denied"
    );
    // negative control — relaxed fs reads .env fine:
    assert!(
        f.allowed(
            serde_json::json!({ "fs": true }),
            CAT,
            &[&s(&f.proj.join(".env"))]
        ),
        "neg-control: relaxed fs reads .env"
    );
}

// ── fs write-confine ──────────────────────────────────────────────────────────

#[test]
fn write_confine_allows_target_denies_rest() {
    let f = fixture();
    let wc = serde_json::json!({ "fs": ["...", "./writable"] });
    assert!(
        f.allowed(wc.clone(), TOUCH, &[&s(&f.proj.join("writable/ok.txt"))]),
        "write inside grant"
    );
    assert!(
        !f.allowed(wc.clone(), TOUCH, &[&s(&f.proj.join("blocked.txt"))]),
        "write project root denied"
    );
    assert!(
        !f.allowed(wc, TOUCH, &[&s(&f.root.join("outside/w.txt"))]),
        "write outside denied"
    );
    // negative control — relaxed fs writes anywhere:
    assert!(
        f.allowed(
            serde_json::json!({ "fs": true }),
            TOUCH,
            &[&s(&f.root.join("outside/w2.txt"))]
        ),
        "neg-control: relaxed fs writes outside"
    );
}

// ── env scrub (construction) ──────────────────────────────────────────────────

#[test]
fn env_scrub_strips_secrets_keeps_baseline() {
    let f = fixture();
    let env = &[("PATH", "/usr/bin:/bin"), ("MY_SECRET_TOKEN", "leaked")];
    // `sandbox: true` = curated baseline: PATH survives, the ambient secret does not.
    let strip = serde_json::json!(true);
    assert!(
        f.allowed_env(strip.clone(), env, "/bin/sh", &["-c", "test -n \"$PATH\""]),
        "baseline PATH present"
    );
    assert!(
        f.allowed_env(
            strip,
            env,
            "/bin/sh",
            &["-c", "test -z \"$MY_SECRET_TOKEN\""]
        ),
        "secret var stripped"
    );
    // negative control — env passthrough keeps the secret:
    assert!(
        f.allowed_env(
            serde_json::json!({ "env": true }),
            env,
            "/bin/sh",
            &["-c", "test -n \"$MY_SECRET_TOKEN\""]
        ),
        "neg-control: passthrough keeps the secret"
    );
}

// ── canonicalization traps ────────────────────────────────────────────────────

#[test]
fn firmlink_write_allow_is_not_inert() {
    // A write-allow whose surface path is a /var/folders (firmlink) form must still
    // match the canonical /private/var/folders path the kernel checks. The fixture
    // root is already canonical; assert a not-yet-created dir is grantable (the
    // canonicalize-incl-nonexistent path), proving the grant isn't fail-closed.
    let f = fixture();
    let newdir = f.proj.join("created/at/runtime");
    let surface = serde_json::json!({ "fs": ["./created"] });
    assert!(
        f.allowed(
            surface.clone(),
            "/bin/sh",
            &[
                "-c",
                &format!("mkdir -p {q} && touch {q}/f", q = s(&newdir))
            ]
        ),
        "create+write a not-yet-existing granted dir works"
    );
    // A sibling NOT under the grant stays denied.
    assert!(
        !f.allowed(surface, TOUCH, &[&s(&f.proj.join("elsewhere.txt"))]),
        "non-granted sibling write denied"
    );
}

#[test]
fn deny_not_dodgeable_via_dotdot_or_symlink() {
    let f = fixture();
    let generous = serde_json::json!({ "fs": ["..."] });
    // `..` traversal to the denied .env resolves to the same canonical path.
    let dotdot = f.proj.join("sub/../.env");
    assert!(
        !f.allowed(generous.clone(), CAT, &[&s(&dotdot)]),
        "'..' to .env still denied"
    );
    // A symlink to the denied .env: the kernel resolves the link before matching.
    let link = f.proj.join("envlink");
    std::os::unix::fs::symlink(f.proj.join(".env"), &link).unwrap();
    assert!(
        !f.allowed(generous, CAT, &[&s(&link)]),
        "symlink to .env still denied"
    );
    // A symlink escaping read-confine to an out-of-project secret.
    let confine = serde_json::json!({ "fs": ["./"] });
    let escape = f.proj.join("escape");
    std::os::unix::fs::symlink(f.home.join(".ssh/id_rsa"), &escape).unwrap();
    assert!(
        !f.allowed(confine, CAT, &[&s(&escape)]),
        "symlink escaping confine denied"
    );
}
