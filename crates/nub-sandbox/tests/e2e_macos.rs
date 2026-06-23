//! Real end-to-end enforcement test for the macOS Seatbelt backend.
//!
//! This is the load-bearing verification: it spawns ACTUAL `sandbox-exec`-
//! wrapped processes and asserts the jail CONTAINS a malicious-shaped script
//! (can't read a seeded secret, can't write outside the package dir, can't
//! egress) while a legit build operation (read project, write package dir)
//! STILL SUCCEEDS. A green unit suite over policy structs is not enough — the
//! design is default-ON and security-critical, so the enforcement must be
//! proven against the real OS sandbox, not just the SBPL string.
//!
//! macOS-only; the Linux equivalent runs under Docker/CI (Landlock needs a
//! live kernel). Run: `cargo test -p nub-sandbox --test e2e_macos`.
#![cfg(target_os = "macos")]

use nub_sandbox::build_jail::{self, BuildJailParams};
use nub_sandbox::{SandboxPolicy, apply_to_command};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a jail policy for a fixture laid out as <root>/{project, home, jail_home}.
fn fixture_policy(root: &Path) -> (SandboxPolicy, PathBuf, PathBuf, PathBuf) {
    let project = root.join("project");
    let package_dir = project.join("node_modules/dep");
    let home = root.join("home");
    let jail_home = root.join("jailhome");
    for d in [&package_dir, &home, &jail_home] {
        fs::create_dir_all(d).unwrap();
    }
    let policy = build_jail::policy(&BuildJailParams {
        package_dir: package_dir.clone(),
        project_root: project.clone(),
        jail_home: jail_home.clone(),
        user_home: home.clone(),
        extra_write: vec![],
        registry_hosts: vec!["registry.npmjs.org".into()],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });
    (policy, project, package_dir, home)
}

/// Run a `sh -c <script>` under the jail; return (exit_ok, stdout+stderr).
fn run_jailed(policy: &SandboxPolicy, cwd: &Path, script: &str) -> (bool, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    // The build-jail re-homes HOME at jail_home; mirror what the embedder does
    // so absolute-secret-path probes resolve against the REAL home (the deny
    // set covers the real home too).
    apply_to_command(&mut cmd, policy).expect("apply jail");
    let out = cmd.output().expect("spawn jailed");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

#[test]
fn legit_build_can_read_project_and_write_package_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, project, package_dir, _home) = fixture_policy(tmp.path());
    // a source file the build legitimately reads
    fs::write(project.join("binding.gyp"), "{}").unwrap();

    // legit: read the project file, write a build artifact into the package dir
    let script = format!(
        "cat {src} && echo built > {out}",
        src = project.join("binding.gyp").display(),
        out = package_dir.join("dep.node").display()
    );
    let (ok, log) = run_jailed(&policy, &package_dir, &script);
    assert!(ok, "legit build was blocked by the jail: {log}");
    assert!(
        package_dir.join("dep.node").exists(),
        "build artifact not written: {log}"
    );
}

#[test]
fn malicious_write_outside_package_dir_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, project, package_dir, home) = fixture_policy(tmp.path());

    // attempt 1: drop a backdoor into the PROJECT SOURCE (read-only)
    let backdoor = project.join("backdoor.js");
    let (_ok, _log) = run_jailed(
        &policy,
        &package_dir,
        &format!("echo pwned > {}", backdoor.display()),
    );
    assert!(
        !backdoor.exists(),
        "jail allowed a write into the read-only project source"
    );

    // attempt 2: persistence write into HOME (e.g. ~/.bashrc-style)
    let rc = home.join(".bashrc");
    run_jailed(
        &policy,
        &package_dir,
        &format!("echo evil >> {}", rc.display()),
    );
    assert!(!rc.exists(), "jail allowed a persistence write into HOME");
}

#[test]
fn malicious_secret_read_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir, home) = fixture_policy(tmp.path());

    // seed a credential in the (real, for-this-test) home secret dir
    let ssh = home.join(".ssh");
    fs::create_dir_all(&ssh).unwrap();
    let key = ssh.join("id_rsa");
    fs::write(&key, "SUPER-SECRET-KEY").unwrap();

    // the script tries to exfiltrate the key to stdout
    let (_ok, log) = run_jailed(&policy, &package_dir, &format!("cat {}", key.display()));
    assert!(
        !log.contains("SUPER-SECRET-KEY"),
        "jail leaked a seeded secret: {log}"
    );
}

#[test]
fn dotenv_read_is_blocked_at_any_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, project, package_dir, _home) = fixture_policy(tmp.path());

    // a .env inside the (readable) project tree must still be read-denied
    let env = project.join(".env");
    fs::write(&env, "DATABASE_PASSWORD=hunter2").unwrap();
    let (_ok, log) = run_jailed(&policy, &package_dir, &format!("cat {}", env.display()));
    assert!(
        !log.contains("hunter2"),
        "jail leaked a project .env secret: {log}"
    );
}

#[test]
fn network_egress_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir, _home) = fixture_policy(tmp.path());

    // a raw TCP beacon to an external host must fail (net fully denied until
    // the proxy lands). Use /dev/tcp via bash if present, else nc; the assert is
    // "no successful connection".
    let (ok, log) = run_jailed(
        &policy,
        &package_dir,
        // bash's /dev/tcp; success would print CONNECTED. The jail denies the
        // outbound socket, so this must fail.
        "exec 3<>/dev/tcp/1.1.1.1/80 && echo CONNECTED || echo BLOCKED",
    );
    // the command itself returns 0 via the `|| echo BLOCKED` branch; what
    // matters is it did NOT connect.
    assert!(
        !log.contains("CONNECTED"),
        "jail allowed an outbound network connection: ok={ok} log={log}"
    );
}
