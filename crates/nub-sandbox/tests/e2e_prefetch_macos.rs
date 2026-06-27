//! Prefetch-sufficiency thesis — the empirical proof that the build-jail's
//! **net-deny-all + prefetch** default is enough for native-package compat.
//!
//! The thesis (`.fray/build-jail-default-on.md`): nub downloads a package's
//! prebuilt artifact OUTSIDE the jail during install resolution ("prefetch"),
//! warming the downloader's cache; the lifecycle script then runs under the jail
//! with NO network and finds the artifact already cached. So the ONLY thing the
//! jail must NOT break is the offline consume-from-cache path, and the ONLY
//! thing it DOES break is a cold network fetch.
//!
//! These two tests prove exactly that, hermetically (a loopback `TcpListener`
//! stands in for the registry/CDN — no external network, runs in CI):
//!   1. WARM — a build that reads its prebuilt from a (prefetch-warmed) cache dir
//!      and writes the artifact into the package dir SUCCEEDS jailed, touching no
//!      network.
//!   2. COLD — a build that must reach the network FAILS jailed, and the SAME
//!      build SUCCEEDS the instant the net axis is lifted (fs + env confinement
//!      unchanged) — the negative control proving the network is the SOLE
//!      blocker, i.e. prefetch (warming the cache) is the whole fix.
//!
//! The real-package validation (esbuild / better-sqlite3 / bcrypt / @swc/core)
//! that motivated this lives in `tests/build-jail/README.md` + the `jail_run`
//! example; it needs real npm installs + network so it is not a CI unit test.
//!
//! macOS-only (Seatbelt). Run: `cargo test -p nub-sandbox --test e2e_prefetch_macos`.
#![cfg(target_os = "macos")]

use nub_sandbox::script_sandbox::{self, ScriptSandboxParams};
use nub_sandbox::{SandboxPolicy, apply_to_command};
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

/// A jail policy for a fixture laid out as <root>/{project, home, sandbox_home},
/// with `cache` (a prefetch-warmed download cache) added to the writable set.
fn fixture(root: &Path) -> (SandboxPolicy, PathBuf, PathBuf) {
    let project = root.join("project");
    let package_dir = project.join("node_modules/dep");
    let home = root.join("home");
    let sandbox_home = root.join("sandboxhome");
    let cache = root.join("cache"); // stands in for ~/.npm/_prebuilds
    for d in [&package_dir, &home, &sandbox_home, &cache] {
        std::fs::create_dir_all(d).unwrap();
    }
    let policy = script_sandbox::policy(&ScriptSandboxParams {
        package_dir: package_dir.clone(),
        project_root: project.clone(),
        sandbox_home,
        user_home: home,
        extra_write: vec![cache.clone()],
        registry_hosts: vec!["registry.npmjs.org".into()],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });
    (policy, package_dir, cache)
}

fn run(policy: &SandboxPolicy, cwd: &Path, script: &str) -> (bool, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    apply_to_command(&mut cmd, policy).expect("apply sandbox");
    let out = cmd.output().expect("spawn sandboxed");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

#[test]
fn warm_cache_offline_build_succeeds_under_jail() {
    // PREFETCH MODEL: the prebuilt artifact is already in the (warmed) cache.
    // The jailed build copies it into the package dir's build output — exactly
    // what prebuild-install/node-pre-gyp do on a cache hit. No network involved;
    // the jail must let this through.
    let tmp = tempfile::tempdir().unwrap();
    let (policy, package_dir, cache) = fixture(tmp.path());
    std::fs::write(cache.join("dep-v1-darwin-arm64.node"), b"PREBUILT-BINARY").unwrap();

    let (ok, log) = run(
        &policy,
        &package_dir,
        &format!(
            "mkdir -p build/Release && cp {src} build/Release/dep.node && cat build/Release/dep.node",
            src = cache.join("dep-v1-darwin-arm64.node").display()
        ),
    );
    assert!(ok, "warm offline build was blocked by the jail: {log}");
    assert!(
        package_dir.join("build/Release/dep.node").exists(),
        "artifact not produced from the warm cache: {log}"
    );
    assert!(
        log.contains("PREBUILT-BINARY"),
        "build did not consume the cached prebuilt: {log}"
    );
}

#[test]
fn cold_network_fetch_blocked_and_net_is_the_sole_blocker() {
    // COLD MODEL: the cache is empty, so the build must hit the "registry/CDN"
    // (a loopback listener here). Two legs share ONE script + ONE policy struct,
    // differing ONLY in policy.net.enforce — so any difference is attributable
    // to the net axis alone.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // accept exactly one connection (the net-lifted leg) off-thread so the
    // connecting child doesn't block on the handshake.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut s = stream;
            let mut buf = [0u8; 1];
            let _ = s.read(&mut buf);
            let _ = tx.send(());
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let (mut policy, package_dir, _cache) = fixture(tmp.path());
    // success of the connect == the artifact would have been fetched; a clean
    // "BLOCKED"/"FETCHED" marker keeps the failure message self-debugging.
    let script = format!("exec 3<>/dev/tcp/127.0.0.1/{port} && echo FETCHED || echo BLOCKED");

    // leg 1 — jailed (net enforced): the fetch is denied.
    policy.net.enforce = true;
    let (_ok, log) = run(&policy, &package_dir, &script);
    assert!(
        log.contains("BLOCKED") && !log.contains("FETCHED"),
        "cold network fetch was NOT blocked by the jail: {log}"
    );

    // leg 2 — net axis lifted (fs + env confinement unchanged): the SAME fetch
    // now succeeds. This is the negative control: it proves the jail's NET axis
    // (not fs/env) is what blocked leg 1 — i.e. warming the cache (prefetch) is
    // the entire fix.
    policy.net.enforce = false;
    let (_ok2, log2) = run(&policy, &package_dir, &script);
    assert!(
        log2.contains("FETCHED"),
        "net-lifted control did NOT connect — the block was not the net axis: {log2}"
    );
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
        "listener never saw the net-lifted connection"
    );
}
