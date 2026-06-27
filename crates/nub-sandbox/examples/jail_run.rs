//! Empirical build-jail runner — run an arbitrary command under the
//! install/build-script sandbox preset, for validating the prefetch-sufficiency
//! and attack-containment theses against REAL native packages.
//!
//! Usage:
//!   jail_run --pkg <package_dir> --root <project_root> [--home <user_home>]
//!            [--no-net-enforce] [--no-env-scrub] -- <cmd> [args...]
//!
//! It assembles the default-ON `script_sandbox::policy` for the given dirs (net
//! fully denied — the proxy is unwired — plus the fs/env/secret confinement),
//! applies the env scrub over the parent env, applies the OS backend, runs the
//! command inheriting stdio, and prints the resolved Degradation + exit status.
//!
//! `--no-net-enforce` / `--no-env-scrub` exist to PROVE a jailed failure is
//! caused by the axis under test (disable it → the same command should now
//! succeed), i.e. to confirm a RED assertion really turns GREEN when the jail
//! is lifted. macOS-focused; works on any OS the backend supports.

use nub_sandbox::script_sandbox::{self, ScriptSandboxParams};
use nub_sandbox::{apply_env_scrub, apply_to_command};
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut pkg: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    let mut home: Option<PathBuf> = None;
    let mut net_enforce = true;
    let mut env_scrub = true;
    let mut cmd: Vec<String> = Vec::new();

    while let Some(a) = args.next() {
        match a.as_str() {
            "--pkg" => pkg = args.next().map(PathBuf::from),
            "--root" => root = args.next().map(PathBuf::from),
            "--home" => home = args.next().map(PathBuf::from),
            "--no-net-enforce" => net_enforce = false,
            "--no-env-scrub" => env_scrub = false,
            "--" => {
                cmd.extend(args.by_ref());
                break;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    let pkg = pkg.expect("--pkg required");
    let root = root.expect("--root required");
    let user_home = home.unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()));
    let sandbox_home = std::env::temp_dir().join(format!("jail-run-home-{}", std::process::id()));
    std::fs::create_dir_all(&sandbox_home).unwrap();

    // The §5 mandatory build caches (node-gyp + _prebuilds) resolve under the
    // REAL home here so a warm cache is found; plus node-pre-gyp's cache dir.
    let mut extra_write =
        script_sandbox::default_extra_write(&user_home, Some(&user_home.join(".npm")));
    extra_write.push(user_home.join(".cache/node-pre-gyp"));
    extra_write.push(user_home.join(".cache/node-gyp"));

    let mut policy = script_sandbox::policy(&ScriptSandboxParams {
        package_dir: pkg.clone(),
        project_root: root.clone(),
        sandbox_home,
        user_home,
        extra_write,
        registry_hosts: vec!["registry.npmjs.org".into()],
        extra_hosts: vec![],
        bundle_browser_cdns: false,
    });
    policy.net.enforce = net_enforce;
    policy.env.enforce = env_scrub;

    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).current_dir(&pkg);

    // Env scrub over the parent env, then re-inject the npm plumbing a lifecycle
    // script needs (npm runs these vars; running the script bare loses them).
    apply_env_scrub(&mut c, &policy.env, std::env::vars());
    if env_scrub {
        // these survive the allowlist already (PATH/HOME/NODE/npm_config_*), but
        // be explicit about NODE so node-gyp/prebuild-install find the runtime.
        if let Ok(p) = std::env::var("PATH") {
            c.env("PATH", p);
        }
    }

    let deg = apply_to_command(&mut c, &policy).expect("apply sandbox");
    eprintln!(
        "[jail_run] net_enforce={net_enforce} env_scrub={env_scrub} degradation={:?}",
        deg
    );

    let status = c.status().expect("spawn");
    eprintln!("[jail_run] exit: {:?}", status.code());
    std::process::exit(status.code().unwrap_or(1));
}
