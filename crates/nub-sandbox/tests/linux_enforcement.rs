//! Linux Landlock/seccomp backend — REAL enforcement tests.
//!
//! Each test compiles a surface policy, applies it, SPAWNS the child under
//! Landlock/seccomp, and asserts the kernel allowed or denied the action. Every
//! confinement assertion is paired with a NEGATIVE CONTROL (the axis lifted → the
//! same action succeeds) so a pass can't be hollow. Linux-only, and a no-op (skips)
//! on a kernel without Landlock — CI runs it on `ubuntu-24.04` (kernel 6.x, Landlock
//! present); the dev host proves it in a Lima/Colima Ubuntu 24.04 VM.
//!
//! Syscall-level axes (net socket-creation, ptrace, process_vm_readv) use a tiny C
//! probe compiled at runtime with `cc`; the seccomp deny is isolated from the host's
//! `yama.ptrace_scope` by targeting SELF (yama always permits self, so a lifted
//! seccomp lets it through — proving the block is ours).
#![cfg(target_os = "linux")]

use nub_sandbox::compiler::{CompileCtx, ShellRunner};
use nub_sandbox::matcher::Homes;
use nub_sandbox::{CommandSpec, apply, compile};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::TempDir;

/// Raw Landlock ABI probe (mirrors the backend's) — skip the suite on a kernel that
/// can't enforce, so a Landlock-less environment reports "ok" rather than red.
fn landlock_available() -> bool {
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u64,
        )
    };
    abi >= 2
}

/// Returns true when the caller should SKIP (no Landlock on this kernel). Under
/// `NUB_SANDBOX_REQUIRE_LANDLOCK=1` — set by the conformance-CI real-kernel leg — a
/// missing Landlock PANICS instead: a hollow skip must never read as green on the
/// runner whose whole job is to prove Landlock enforces there.
fn skip_without_landlock() -> bool {
    if landlock_available() {
        return false;
    }
    let required = matches!(
        std::env::var("NUB_SANDBOX_REQUIRE_LANDLOCK").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    assert!(
        !required,
        "NUB_SANDBOX_REQUIRE_LANDLOCK set but this kernel exposes no Landlock ABI>=2 — \
         enforcement cannot be proven here (conformance real-kernel gate)"
    );
    true
}

struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    proj: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let proj = root.join("proj");
    let home = root.join("home");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::create_dir_all(proj.join("writable")).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(proj.join("pub.txt"), "PUBLIC").unwrap();
    std::fs::write(proj.join("sub/nested.txt"), "N").unwrap();
    std::fs::write(proj.join(".env"), "ENVSECRET").unwrap();
    std::fs::write(proj.join("sub/.env"), "NESTEDENV").unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "IDRSA").unwrap();
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
        CompileCtx {
            homes: self.homes(),
            cwd: self.proj.clone(),
            trusted: true,
            ambient_env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            runner: Box::new(ShellRunner),
        }
    }

    /// Spawn `program args…` under `surface` (with `env` as the ambient snapshot) and
    /// return `(exit_code, stdout)`. stderr → null; stdout captured for the /proc test.
    fn run(
        &self,
        surface: Value,
        env: &[(&str, &str)],
        program: &str,
        args: &[&str],
    ) -> (i32, String) {
        let policy = compile(&surface, &self.ctx(env)).expect("compiles");
        let spec = CommandSpec::new(program)
            .args(args.iter().copied())
            .cwd(&self.proj);
        let prepared = apply(&policy, spec).expect("apply");
        let mut cmd = prepared.command;
        cmd.stderr(Stdio::null()).stdin(Stdio::null());
        let out = cmd.output().expect("spawn");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn ok(&self, surface: Value, program: &str, args: &[&str]) -> bool {
        self.run(surface, &[], program, args).0 == 0
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

const CAT: &str = "/bin/cat";
const TOUCH: &str = "/usr/bin/touch";
const SH: &str = "/bin/sh";

// ── fs read-confine (allowlist) ────────────────────────────────────────────────

#[test]
fn read_confine_allows_project_denies_outside() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    let confine = serde_json::json!({ "fs": ["./"] });
    assert!(
        f.ok(confine.clone(), CAT, &[&s(&f.proj.join("pub.txt"))]),
        "project read"
    );
    assert!(
        f.ok(confine.clone(), CAT, &[&s(&f.proj.join("sub/nested.txt"))]),
        "nested read"
    );
    assert!(
        !f.ok(confine.clone(), CAT, &[&s(&f.home.join(".ssh/id_rsa"))]),
        "home secret denied"
    );
    // negative control — relaxed fs reads the secret fine:
    assert!(
        f.ok(
            serde_json::json!({ "fs": true }),
            CAT,
            &[&s(&f.home.join(".ssh/id_rsa"))]
        ),
        "neg-control: relaxed fs reads outside"
    );
}

#[test]
fn generous_read_denies_dotenv() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // Bounded generous read: allow the fixture root, deny .env at any depth.
    let surface = serde_json::json!({ "fs": [s(&f.root), "!**/.env"] });
    assert!(
        f.ok(surface.clone(), CAT, &[&s(&f.proj.join("pub.txt"))]),
        "pub readable"
    );
    assert!(
        !f.ok(surface.clone(), CAT, &[&s(&f.proj.join(".env"))]),
        ".env denied"
    );
    assert!(
        !f.ok(surface, CAT, &[&s(&f.proj.join("sub/.env"))]),
        "nested .env denied"
    );
    assert!(
        f.ok(
            serde_json::json!({ "fs": true }),
            CAT,
            &[&s(&f.proj.join(".env"))]
        ),
        "neg-control: relaxed fs reads .env"
    );
}

// ── new secret deny-set additions (A2): each denied under a generous read ──────

#[test]
fn new_secret_paths_denied_under_generous_read() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // Home-anchored secret files/dirs (the deny-set additions), anchored to the
    // fixture home so the walk carves fixture paths, never the real `~`.
    std::fs::write(f.home.join(".pgpass"), "PGPASS").unwrap();
    std::fs::write(f.home.join(".pypirc"), "PYPIRC").unwrap();
    std::fs::create_dir_all(f.home.join(".gnupg")).unwrap();
    std::fs::write(f.home.join(".gnupg/secring.gpg"), "GPGKEY").unwrap();
    std::fs::create_dir_all(f.home.join(".config/git")).unwrap();
    std::fs::write(f.home.join(".config/git/credentials"), "GITCREDS").unwrap();
    // Project-local: `.env` + `.env.local` DIRECTORY subtrees (not just leaves) + direnv
    // `.envrc`.
    std::fs::create_dir_all(f.proj.join("nested/.env")).unwrap();
    std::fs::write(f.proj.join("nested/.env/prod"), "ENVDIRSECRET").unwrap();
    std::fs::create_dir_all(f.proj.join("nested/.env.local")).unwrap();
    std::fs::write(f.proj.join("nested/.env.local/prod"), "ENVLOCALDIRSECRET").unwrap();
    std::fs::write(f.proj.join(".envrc"), "export SECRET=x").unwrap();

    let generous = serde_json::json!({ "fs": ["..."] });
    for (path, label) in [
        (f.home.join(".pgpass"), ".pgpass"),
        (f.home.join(".pypirc"), ".pypirc"),
        (f.home.join(".gnupg/secring.gpg"), ".gnupg subtree"),
        (
            f.home.join(".config/git/credentials"),
            "git credential store",
        ),
        (f.proj.join("nested/.env/prod"), ".env directory subtree"),
        (
            f.proj.join("nested/.env.local/prod"),
            ".env.local directory subtree",
        ),
        (f.proj.join(".envrc"), ".envrc"),
    ] {
        assert!(
            !f.ok(generous.clone(), CAT, &[&s(&path)]),
            "{label} must be denied under generous read"
        );
    }
    // negative controls — relaxed fs reads each fine, so the deny (not a missing
    // file) is the cause above. Cover a home file + both new glob mechanisms.
    for path in [
        f.home.join(".pgpass"),
        f.proj.join("nested/.env/prod"),
        f.proj.join(".envrc"),
    ] {
        assert!(
            f.ok(serde_json::json!({ "fs": true }), CAT, &[&s(&path)]),
            "neg-control: relaxed fs reads {}",
            s(&path)
        );
    }
}

// ── fs write-confine ────────────────────────────────────────────────────────────

#[test]
fn write_confine_allows_target_denies_rest() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    let wc = serde_json::json!({ "fs": ["...", "./writable"] });
    assert!(
        f.ok(wc.clone(), TOUCH, &[&s(&f.proj.join("writable/ok.txt"))]),
        "write inside grant"
    );
    assert!(
        !f.ok(wc.clone(), TOUCH, &[&s(&f.proj.join("blocked.txt"))]),
        "write project root denied"
    );
    assert!(
        !f.ok(wc, TOUCH, &[&s(&f.home.join("w.txt"))]),
        "write outside denied"
    );
    assert!(
        f.ok(
            serde_json::json!({ "fs": true }),
            TOUCH,
            &[&s(&f.home.join("w2.txt"))]
        ),
        "neg-control: relaxed fs writes outside"
    );
}

// ── adversarial: a symlink or `..` must not dodge read-confine ──────────────────

#[test]
fn confine_not_dodgeable_via_symlink_or_dotdot() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // A symlink inside the confined project pointing OUT to the home ssh key: Landlock
    // checks the RESOLVED inode (the key, not granted) → the read is denied.
    let link = f.proj.join("escape");
    std::os::unix::fs::symlink(f.home.join(".ssh/id_rsa"), &link).unwrap();
    let confine = serde_json::json!({ "fs": ["./"] });
    assert!(
        !f.ok(confine.clone(), CAT, &[&s(&link)]),
        "symlink escaping read-confine denied"
    );
    // A `..` traversal out of the project to the same key is likewise denied.
    let dotdot = f.proj.join("../home/.ssh/id_rsa");
    assert!(!f.ok(confine, CAT, &[&s(&dotdot)]), "'..' escape denied");
    // negative control — relaxed fs follows the symlink fine.
    assert!(
        f.ok(serde_json::json!({ "fs": true }), CAT, &[&s(&link)]),
        "neg-control: relaxed fs follows the symlink"
    );
}

// ── the env-read boundary: /proc/<ancestor>/environ ─────────────────────────────

#[test]
fn ancestor_proc_environ_is_unreadable() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // The ancestor is THIS test process; its REAL environ holds `PATH=`. The child
    // tries to read the ancestor's `/proc/<ppid>/environ`; `$PPID` is the shell's
    // parent = the test process (which spawns sh directly). This isolates the
    // READ-CONFINE mechanism (no env axis): under `{fs:["./"]}`, /proc is never
    // granted → the read is denied. (The env-scrub mechanism — a scrubbed child
    // recovering the ancestor env — is proven separately by
    // env_scrub_alone_closes_proc_even_with_fs_relaxed.)
    let read_environ = "/bin/cat /proc/$PPID/environ 2>/dev/null || true";
    let (_c, out) = f.run(
        serde_json::json!({ "fs": ["./"] }),
        &[],
        SH,
        &["-c", read_environ],
    );
    assert!(
        !out.contains("PATH="),
        "ancestor environ must be unreadable under read-confine (got {} bytes)",
        out.len()
    );
    // negative control — nothing enforced (relaxed fs, no env scrub) → readable.
    let (_c2, out2) = f.run(
        serde_json::json!({ "fs": true }),
        &[],
        SH,
        &["-c", read_environ],
    );
    assert!(
        out2.contains("PATH="),
        "neg-control: relaxed fs CAN read the ancestor environ"
    );
}

#[test]
fn env_scrub_alone_closes_proc_even_with_fs_relaxed() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // A pure env-scrub with fs RELAXED must STILL close /proc — otherwise the
    // scrubbed child recovers the ancestor's env via /proc/<ppid>/environ, defeating
    // the scrub. `{env:false}` with a non-empty ambient WITHHOLDS a var, so the
    // backend installs a relaxed-minus-/proc Landlock ruleset for the env-read
    // boundary (fs stays relaxed; only /proc,/sys are closed).
    let read_environ = "/bin/cat /proc/$PPID/environ 2>/dev/null || true";
    // `fs: true` is now NAMED explicitly: under the complete-statement model an
    // unlisted axis FLOORS (deny-all), so `{ env: false }` alone would confine fs
    // and the probe could not exec — this test wants env scrub with fs RELAXED.
    let (_c, out) = f.run(
        serde_json::json!({ "env": false, "fs": true }),
        &[("AMBIENT_SECRET", "s")],
        SH,
        &["-c", read_environ],
    );
    assert!(
        !out.contains("PATH="),
        "env-scrub must close /proc even with fs relaxed (got {} bytes)",
        out.len()
    );
    // negative control — nothing enforced → /proc readable. `sandbox: false` is the
    // unambiguous "fully unjailed" surface (a bare `{ fs: true }` now floors net+env).
    let (_c2, out2) = f.run(serde_json::json!(false), &[], SH, &["-c", read_environ]);
    assert!(
        out2.contains("PATH="),
        "neg-control: unsandboxed reads /proc"
    );
}

#[test]
fn env_passthrough_does_not_close_proc() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // `{env:true}` is a PASSTHROUGH: nothing is withheld, so there is no ancestor
    // secret to protect and /proc must stay open (a config the user chose to be
    // permissive) — the `/proc` close only fires when the scrub actually withholds.
    let read_environ = "/bin/cat /proc/$PPID/environ 2>/dev/null || true";
    // fs named RELAXED explicitly (an unlisted axis now floors → the probe couldn't
    // exec); this test is about env passthrough NOT closing /proc.
    let (_c, out) = f.run(
        serde_json::json!({ "env": true, "fs": true }),
        &[("FOO", "bar")],
        SH,
        &["-c", read_environ],
    );
    assert!(
        out.contains("PATH="),
        "env passthrough must NOT close /proc (nothing withheld)"
    );
}

// ── env-scrub (construction) ────────────────────────────────────────────────────

#[test]
fn env_scrub_strips_secret_keeps_baseline() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    let env = &[("PATH", "/usr/bin:/bin"), ("MY_SECRET_TOKEN", "leaked")];
    let strip = serde_json::json!(true); // curated baseline (wrapper true → generous-read fs)
    assert!(
        f.run(strip.clone(), env, SH, &["-c", "test -n \"$PATH\""])
            .0
            == 0,
        "PATH kept"
    );
    assert!(
        f.run(strip, env, SH, &["-c", "test -z \"$MY_SECRET_TOKEN\""])
            .0
            == 0,
        "secret stripped"
    );
    // fs named RELAXED (an unlisted axis floors → SH couldn't exec); this checks
    // that axis `env: true` passthrough keeps the ambient secret.
    assert!(
        f.run(
            serde_json::json!({ "env": true, "fs": true }),
            env,
            SH,
            &["-c", "test -n \"$MY_SECRET_TOKEN\""]
        )
        .0 == 0,
        "neg-control: passthrough keeps the secret"
    );
}

// ── seccomp: net + ptrace family (via a compiled C probe) ───────────────────────

/// Compile the syscall probe once; `None` if `cc` is unavailable (probe tests skip).
fn probe_bin(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("probe.c");
    std::fs::write(&src, PROBE_C).ok()?;
    let bin = dir.join("probe");
    let ok = std::process::Command::new("cc")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .ok()?
        .success();
    ok.then_some(bin)
}

/// Probe: `probe <socket|vmread|ptrace>` exits 42 when the syscall was DENIED
/// (EPERM), 0 when it went through. vmread/ptrace target SELF so the verdict is the
/// seccomp filter's, not the host's yama policy.
const PROBE_C: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <unistd.h>
int main(int argc, char** argv) {
    if (argc < 2) return 2;
    if (!strcmp(argv[1], "socket")) {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0 && errno == EPERM) return 42;
        return 0;
    }
    if (!strcmp(argv[1], "vmread")) {
        char buf[8]; char src[8] = "abcdefg";
        struct iovec l = { buf, sizeof buf }, r = { src, sizeof src };
        long n = process_vm_readv(getpid(), &l, 1, &r, 1, 0);
        if (n < 0 && errno == EPERM) return 42;
        return 0;
    }
    if (!strcmp(argv[1], "ptrace")) {
        long r = ptrace(PTRACE_TRACEME, 0, 0, 0);
        if (r == -1 && errno == EPERM) return 42;
        return 0;
    }
    return 2;
}
"#;

#[test]
fn seccomp_denies_net_ptrace_and_vmread() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    let Some(probe) = probe_bin(&f.proj) else {
        return; // no cc — skip the syscall probes
    };
    let probe = s(&probe);

    // net enforce → socket(AF_INET) blocked; fs relaxed so the probe still runs.
    // `fs: true` is NAMED explicitly now (an unlisted axis floors to deny-all, which
    // would stop the probe from exec'ing at all).
    let net_deny = serde_json::json!({ "net": false, "fs": true });
    assert_eq!(
        f.run(net_deny.clone(), &[], &probe, &["socket"]).0,
        42,
        "AF_INET socket denied"
    );
    // ptrace + process_vm_readv denied whenever sandboxing (here via net enforce).
    assert_eq!(
        f.run(net_deny.clone(), &[], &probe, &["ptrace"]).0,
        42,
        "ptrace denied"
    );
    assert_eq!(
        f.run(net_deny, &[], &probe, &["vmread"]).0,
        42,
        "process_vm_readv denied"
    );

    // negative control — no sandbox → every syscall goes through. `sandbox: false`
    // is the unambiguous fully-unjailed surface (a bare `{ fs: true }` now floors
    // net → seccomp would block the socket, defeating the control).
    let relaxed = serde_json::json!(false);
    assert_eq!(
        f.run(relaxed.clone(), &[], &probe, &["socket"]).0,
        0,
        "neg: socket allowed unsandboxed"
    );
    assert_eq!(
        f.run(relaxed.clone(), &[], &probe, &["ptrace"]).0,
        0,
        "neg: ptrace allowed unsandboxed"
    );
    assert_eq!(
        f.run(relaxed, &[], &probe, &["vmread"]).0,
        0,
        "neg: vmread allowed unsandboxed"
    );
}

// ── graceful degradation is honest ──────────────────────────────────────────────

#[test]
fn enforcement_is_full_on_a_landlock_kernel() {
    if skip_without_landlock() {
        return;
    }
    let f = fixture();
    // A read-confine with no per-host net allow enforces fully — the Degradation
    // must be empty (no silent loss). (The Landlock-unavailable degrade path is
    // covered by the parent-side logic + the LinuxKit case, not reachable here.)
    let policy = compile(&serde_json::json!({ "fs": ["./"] }), &f.ctx(&[])).unwrap();
    let prepared = apply(&policy, CommandSpec::new(CAT).cwd(&f.proj)).unwrap();
    assert!(
        prepared.degradation.is_full(),
        "read-confine fully enforces on a Landlock kernel"
    );
}
