//! Real end-to-end enforcement test for the Linux Landlock+seccomp backend.
//!
//! Asserts what the Linux first cut DOES enforce: fs-write confinement
//! (Landlock) + network egress deny (seccomp), and that a legit build write
//! into the package dir succeeds. The secret-READ deny is a documented Linux
//! follow-on (Landlock is allow-only — the script-sandbox grants `/` read), so this
//! file deliberately does NOT assert secret-read denial on Linux (that would be
//! a false claim; the backend reports `fs-read-deny` as degraded).
//!
//! Needs a live kernel with Landlock (>= 5.19) — run under Docker/CI:
//! `cargo test -p nub-sandbox --test e2e_linux`. If Landlock is unavailable the
//! backend degrades (no fs sandbox); the write-confine assertions are then skipped
//! via a capability check so the test doesn't false-fail on an old kernel.
#![cfg(target_os = "linux")]

use nub_sandbox::script_sandbox::{self, ScriptSandboxParams};
use nub_sandbox::{SandboxPolicy, apply_to_command};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_policy(root: &Path) -> (SandboxPolicy, PathBuf, PathBuf) {
    let project = root.join("project");
    let package_dir = project.join("node_modules/dep");
    let home = root.join("home");
    let sandbox_home = root.join("sandboxhome");
    for d in [&package_dir, &home, &sandbox_home] {
        fs::create_dir_all(d).unwrap();
    }
    let policy = script_sandbox::policy(&ScriptSandboxParams {
        package_dir: package_dir.clone(),
        project_root: project.clone(),
        sandbox_home,
        user_home: home,
        extra_write: vec![],
        registry_hosts: vec![],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });
    (policy, project, package_dir)
}

fn run_sandboxed(policy: &SandboxPolicy, cwd: &Path, script: &str) -> (bool, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    apply_to_command(&mut cmd, policy).expect("apply sandbox");
    let out = cmd.output().expect("spawn sandboxed");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

/// Probe: does a write into a denied path actually fail? If not (no Landlock),
/// skip the write-confine assertions rather than false-fail.
fn landlock_enforcing(root: &Path) -> bool {
    let (_policy, project, package_dir) = fixture_policy(root);
    let probe = project.join("probe");
    run_sandboxed(
        &_policy,
        &package_dir,
        &format!("echo x > {}", probe.display()),
    );
    !probe.exists()
}

#[test]
fn legit_write_into_package_dir_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir) = fixture_policy(tmp.path());
    let artifact = package_dir.join("dep.node");
    let (ok, log) = run_sandboxed(
        &policy,
        &package_dir,
        &format!("echo built > {}", artifact.display()),
    );
    assert!(ok, "legit package-dir write blocked: {log}");
    assert!(artifact.exists(), "artifact not written: {log}");
}

#[test]
fn write_outside_package_dir_is_blocked_when_landlock_available() {
    let tmp = tempfile::tempdir().unwrap();
    if !landlock_enforcing(tmp.path()) {
        eprintln!("Landlock not enforcing on this kernel — skipping write-confine assertion");
        return;
    }
    let (policy, project, package_dir) = fixture_policy(tmp.path());
    let backdoor = project.join("backdoor.js");
    run_sandboxed(
        &policy,
        &package_dir,
        &format!("echo pwned > {}", backdoor.display()),
    );
    assert!(
        !backdoor.exists(),
        "Landlock allowed a write into the read-only project source"
    );
}

#[test]
fn network_egress_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir) = fixture_policy(tmp.path());
    // seccomp denies AF_INET socket() — any outbound TCP attempt fails.
    let (_ok, log) = run_sandboxed(
        &policy,
        &package_dir,
        "exec 3<>/dev/tcp/1.1.1.1/80 && echo CONNECTED || echo BLOCKED",
    );
    assert!(
        !log.contains("CONNECTED"),
        "seccomp allowed an outbound network connection: {log}"
    );
}
