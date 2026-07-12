//! `nub run` through the bundled cross-platform shell emulator
//! (`deno_task_shell`), which is the DEFAULT script engine on every platform.
//! These run end-to-end through the binary against throwaway fixtures — no
//! install needed (the scripts are bare `rm`/`echo`/`node`), so they run
//! offline. The real payoff is the Windows CI leg: a POSIX script that `cmd.exe`
//! could never run now works there because it routes through the emulator.
//!
//! Contracts pinned:
//!   - a POSIX-ism script (`rm -rf` + `&&` + redirection) runs green by default;
//!   - nub's augmentation + lifecycle env reaches the emulated script;
//!   - forwarded args are POSIX-escaped and arrive as single literal tokens;
//!   - a child process's exit code propagates through the emulator;
//!   - a trailing-`&` background job runs (proves the tokio `LocalSet` is wired
//!     — deno's `spawn_local` panics without it);
//!   - `shellEmulator=false` opts back out to the native shell.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // <profile>/
    path.push("nub");
    path
}

fn tmp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nub-emul-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Run `nub <args>` in `dir` with global caches redirected to temp dirs so the
/// run can't read or write the host's real config/cache. `NODE_OPTIONS` is
/// cleared so the harness's own value can't masquerade as one nub injected.
fn run_nub(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(nub_binary())
        .args(args)
        .current_dir(dir)
        .env_remove("NODE_OPTIONS")
        .env("XDG_DATA_HOME", tmp_dir("xdg-data"))
        .env("XDG_CACHE_HOME", tmp_dir("xdg-cache"))
        .output()
        .expect("failed to spawn nub");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn write_manifest(root: &Path, scripts: &str) {
    write(
        &root.join("package.json"),
        &format!(r#"{{"name":"emul-fixture","version":"1.0.0","scripts":{scripts}}}"#),
    );
}

#[test]
fn posix_script_runs_green_under_default_emulator() {
    // The headline case: a script that mixes `rm -rf`, `&&`, `mkdir`, and output
    // redirection — none of which `cmd.exe` runs — succeeds by default because it
    // routes through the emulator on every platform.
    let root = tmp_dir("posix");
    write_manifest(
        &root,
        r#"{"clean":"rm -rf dist && mkdir dist && echo ok > dist/marker"}"#,
    );

    let (_out, err, code) = run_nub(&root, &["run", "clean"]);
    assert_eq!(
        code, 0,
        "clean script should succeed under the emulator: {err}"
    );
    let marker = std::fs::read_to_string(root.join("dist/marker")).expect("dist/marker written");
    assert_eq!(marker.trim(), "ok");
}

#[test]
fn augmentation_env_reaches_emulated_script() {
    // The load-bearing property: nub's augmentation + lifecycle env must reach
    // the emulator's env map (deno spawns children with `env_clear`, so anything
    // missing from the map is missing from a `node`/`tsc` launched by a script).
    // `$NODE` (nub points it at the Node running the script), `$npm_lifecycle_event`
    // (the script name), and `$npm_config_registry` are always set by the shared
    // env assembly regardless of Node version — unlike `NODE_OPTIONS`, which nub
    // leaves empty on Nodes new enough to need no injected flags.
    let root = tmp_dir("augenv");
    write_manifest(
        &root,
        r#"{"probe":"echo evt=$npm_lifecycle_event node=[$NODE] reg=$npm_config_registry"}"#,
    );

    let (out, err, code) = run_nub(&root, &["run", "probe"]);
    assert_eq!(code, 0, "probe script failed: {err}");
    assert!(
        out.contains("evt=probe"),
        "npm_lifecycle_event missing: {out:?}"
    );
    assert!(
        !out.contains("node=[]"),
        "nub's $NODE override should reach the emulated script: {out:?}"
    );
    assert!(
        out.contains("reg=http"),
        "npm_config_registry should reach the emulated script: {out:?}"
    );
}

#[test]
fn forwarded_args_are_posix_escaped_as_single_tokens() {
    // Args after `--` are spliced onto the body with the POSIX (`sh`) escaper on
    // every platform, so a metachar/space arg reaches the script as ONE literal
    // token. `node -e` leaves no script-path element in argv, so the forwarded
    // args are argv[1..] (slice(1)); joining by `|` makes token boundaries visible.
    let root = tmp_dir("args");
    write_manifest(
        &root,
        r#"{"echoargs":"node -e \"console.log(process.argv.slice(1).join('|'))\""}"#,
    );

    let (out, err, code) = run_nub(&root, &["run", "echoargs", "--", "a b", "c"]);
    assert_eq!(code, 0, "echoargs failed: {err}");
    assert!(
        out.contains("a b|c"),
        "spaced arg should arrive as one token, got stdout: {out:?}"
    );
}

#[test]
fn exit_code_propagates_from_emulated_child() {
    // A child process's exit code flows back through the emulator unchanged.
    let root = tmp_dir("exit");
    write_manifest(&root, r#"{"boom":"node -e \"process.exit(3)\""}"#);

    let (_out, err, code) = run_nub(&root, &["run", "boom"]);
    assert_eq!(code, 3, "expected the child's exit 3 to propagate: {err}");
}

#[test]
fn trailing_background_job_runs_without_panicking() {
    // A trailing `&` makes deno `spawn_local` the job, which PANICS unless the
    // executor runs inside a tokio `LocalSet`. Exit 0 (not an abort code) proves
    // the LocalSet entry is wired.
    let root = tmp_dir("bg");
    write_manifest(&root, r#"{"bg":"echo hi &"}"#);

    let (_out, err, code) = run_nub(&root, &["run", "bg"]);
    assert_eq!(
        code, 0,
        "trailing-& job should run under the LocalSet, not panic: {err}"
    );
}

/// The opt-out flips the engine from the emulator back to the platform shell.
/// Unix-only: the differentiator is a `for` loop, which the deno subset can't
/// parse but POSIX `sh` runs — so the default (emulator) yields no `abc`, and
/// `shellEmulator=false` (native `sh`) prints it. On Windows the native shell is
/// `cmd`, whose syntax differs, so this precise differential is Unix-scoped.
#[cfg(unix)]
#[test]
fn shell_emulator_false_opts_out_to_native_shell() {
    let root = tmp_dir("optout");
    write_manifest(&root, r#"{"loop":"for i in a b c; do printf $i; done"}"#);

    // Default (emulator): the `for` construct isn't in the subset, so it does not
    // print `abc` — proving the default engine is the emulator, not native `sh`.
    let (out_default, _e, _c) = run_nub(&root, &["run", "loop"]);
    assert!(
        !out_default.contains("abc"),
        "default emulator should not run a POSIX `for` loop, got: {out_default:?}"
    );

    // Opt out → native `sh` runs the loop and prints `abc`.
    write(&root.join(".npmrc"), "shell-emulator=false\n");
    let (out_native, err, code) = run_nub(&root, &["run", "loop"]);
    assert_eq!(code, 0, "native sh loop failed: {err}");
    assert!(
        out_native.contains("abc"),
        "shellEmulator=false should run the loop on native sh, got: {out_native:?}"
    );
}
