//! Real end-to-end enforcement test for the macOS Seatbelt backend.
//!
//! This is the load-bearing verification: it spawns ACTUAL `sandbox-exec`-
//! wrapped processes and asserts the sandbox CONTAINS a malicious-shaped script
//! (can't read a seeded secret, can't write outside the package dir, can't
//! egress) while a legit build operation (read project, write package dir)
//! STILL SUCCEEDS. A green unit suite over policy structs is not enough — the
//! design is default-ON and security-critical, so the enforcement must be
//! proven against the real OS sandbox, not just the SBPL string.
//!
//! macOS-only; the Linux equivalent runs under Docker/CI (Landlock needs a
//! live kernel). Run: `cargo test -p nub-sandbox --test e2e_macos`.
#![cfg(target_os = "macos")]

use nub_sandbox::script_sandbox::{self, ScriptSandboxParams};
use nub_sandbox::{SandboxPolicy, apply_to_command};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a sandbox policy for a fixture laid out as <root>/{project, home, sandbox_home}.
fn fixture_policy(root: &Path) -> (SandboxPolicy, PathBuf, PathBuf, PathBuf) {
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
        sandbox_home: sandbox_home.clone(),
        user_home: home.clone(),
        extra_write: vec![],
        registry_hosts: vec!["registry.npmjs.org".into()],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });
    (policy, project, package_dir, home)
}

/// Run a `sh -c <script>` under the sandbox (fs/net only); return (exit_ok, log).
fn run_sandboxed(policy: &SandboxPolicy, cwd: &Path, script: &str) -> (bool, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    apply_to_command(&mut cmd, policy).expect("apply sandbox");
    let out = cmd.output().expect("spawn sandboxed");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

/// Run a `sh -c <script>` with the FULL sandbox incl. the env-axis scrub applied
/// against `inherited` (then injected plumbing), exactly the embedder path. This
/// is the only helper that exercises the env axis end-to-end.
fn run_sandboxed_with_env(
    policy: &SandboxPolicy,
    cwd: &Path,
    inherited: Vec<(String, String)>,
    script: &str,
) -> (bool, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    // env-scrub FIRST (clears + re-admits the allowlist), then the OS backend
    // wrap — the documented order.
    nub_sandbox::apply_env_scrub(&mut cmd, &policy.env, inherited);
    apply_to_command(&mut cmd, policy).expect("apply sandbox");
    let out = cmd.output().expect("spawn sandboxed");
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
    let (ok, log) = run_sandboxed(&policy, &package_dir, &script);
    assert!(ok, "legit build was blocked by the sandbox: {log}");
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
    let (_ok, _log) = run_sandboxed(
        &policy,
        &package_dir,
        &format!("echo pwned > {}", backdoor.display()),
    );
    assert!(
        !backdoor.exists(),
        "sandbox allowed a write into the read-only project source"
    );

    // attempt 2: persistence write into HOME (e.g. ~/.bashrc-style)
    let rc = home.join(".bashrc");
    run_sandboxed(
        &policy,
        &package_dir,
        &format!("echo evil >> {}", rc.display()),
    );
    assert!(
        !rc.exists(),
        "sandbox allowed a persistence write into HOME"
    );
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
    let (_ok, log) = run_sandboxed(&policy, &package_dir, &format!("cat {}", key.display()));
    assert!(
        !log.contains("SUPER-SECRET-KEY"),
        "sandbox leaked a seeded secret: {log}"
    );
}

#[test]
fn dotenv_read_is_blocked_at_any_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, project, package_dir, _home) = fixture_policy(tmp.path());

    // a .env inside the (readable) project tree must still be read-denied
    let env = project.join(".env");
    fs::write(&env, "DATABASE_PASSWORD=hunter2").unwrap();
    let (_ok, log) = run_sandboxed(&policy, &package_dir, &format!("cat {}", env.display()));
    assert!(
        !log.contains("hunter2"),
        "sandbox leaked a project .env secret: {log}"
    );
}

#[test]
fn network_egress_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir, _home) = fixture_policy(tmp.path());

    // a raw TCP beacon to an external host must fail (net fully denied until
    // the proxy lands). Use /dev/tcp via bash if present, else nc; the assert is
    // "no successful connection".
    let (ok, log) = run_sandboxed(
        &policy,
        &package_dir,
        // bash's /dev/tcp; success would print CONNECTED. The sandbox denies the
        // outbound socket, so this must fail.
        "exec 3<>/dev/tcp/1.1.1.1/80 && echo CONNECTED || echo BLOCKED",
    );
    // the command itself returns 0 via the `|| echo BLOCKED` branch; what
    // matters is it did NOT connect.
    assert!(
        !log.contains("CONNECTED"),
        "sandbox allowed an outbound network connection: ok={ok} log={log}"
    );
}

#[test]
fn parent_env_secret_does_not_leak_into_sandboxed_script() {
    // Regression for the macOS sandbox-exec wrap env-leak (the wrapped Command
    // would otherwise inherit this process's FULL env). The scrubbed allowlist
    // must be the WHOLE child env — a parent secret must be ABSENT.
    let tmp = tempfile::tempdir().unwrap();
    let (policy, _project, package_dir, _home) = fixture_policy(tmp.path());

    let inherited = vec![
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
        (
            "AWS_SECRET_ACCESS_KEY".to_string(),
            "LEAKED-SECRET".to_string(),
        ),
        ("NPM_TOKEN".to_string(), "LEAKED-TOKEN".to_string()),
        ("GH_PAT".to_string(), "LEAKED-PAT".to_string()),
    ];
    let (_ok, log) = run_sandboxed_with_env(
        &policy,
        &package_dir,
        inherited,
        "echo S=$AWS_SECRET_ACCESS_KEY T=$NPM_TOKEN P=$GH_PAT",
    );
    assert!(
        !log.contains("LEAKED"),
        "parent secret env leaked into the sandboxed script: {log}"
    );
}

#[test]
fn node_gyp_style_cache_write_succeeds_even_when_dir_absent() {
    // The §5-mandatory carve-out: a build writes into ~/.cache/node-gyp, which on
    // a COLD cache does not exist at sandbox-apply time. apply_to_command must
    // pre-create the confined write roots so the grant lands and the write
    // succeeds (regression for the canonicalize-on-missing-path silent-deny bug).
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let package_dir = project.join("node_modules/dep");
    let home = tmp.path().join("home");
    let sandbox_home = tmp.path().join("sandboxhome");
    for d in [&package_dir, &home, &sandbox_home] {
        fs::create_dir_all(d).unwrap();
    }
    // node-gyp cache dir DELIBERATELY not created — simulate a cold cache.
    let gyp_cache = home.join(".cache/node-gyp");
    assert!(!gyp_cache.exists());

    let policy = script_sandbox::policy(&ScriptSandboxParams {
        package_dir: package_dir.clone(),
        project_root: project.clone(),
        sandbox_home: sandbox_home.clone(),
        user_home: home.clone(),
        extra_write: script_sandbox::default_extra_write(&home, None),
        registry_hosts: vec![],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });

    let target = gyp_cache.join("26.0.0/node.lib");
    let (ok, log) = run_sandboxed(
        &policy,
        &package_dir,
        &format!(
            "mkdir -p {dir} && echo hdr > {f}",
            dir = gyp_cache.join("26.0.0").display(),
            f = target.display()
        ),
    );
    assert!(ok, "node-gyp cache write was blocked by the sandbox: {log}");
    assert!(target.exists(), "node-gyp cache file not written: {log}");
}
