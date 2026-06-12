//! Process-identity fidelity: `nub` must spawn node with argv0 set to `"node"`
//! so the spawned process reports `process.title` and `process.argv0` as
//! `"node"`, matching plain `node` invoked by PATH name.  `process.execPath`
//! must remain the absolute binary path — it is derived from the resolved
//! executable, not from argv0, so the two invariants are independent.
//!
//! Applies to both the augmented path and `--node` compat mode.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    path.push("nub");
    path
}

fn fixtures_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest).join("../../tests/fixtures")
}

/// Run `nub [extra_args] <fixture>` and parse the JSON the fixture emits.
fn run_identity(extra_args: &[&str]) -> serde_json::Value {
    let fixture = fixtures_dir().join("process-identity/identity.js");
    let output = Command::new(nub_binary())
        .args(extra_args)
        .arg(&fixture)
        .current_dir(fixture.parent().unwrap())
        .output()
        .expect("failed to spawn nub");
    assert!(
        output.status.success(),
        "nub exited {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).expect("fixture must emit valid JSON")
}

/// Augmented mode: `process.title` and `process.argv0` must be `"node"`.
/// `process.execPath` must be an absolute path (not `"node"`).
#[test]
fn augmented_spawn_reports_node_as_title_and_argv0() {
    let v = run_identity(&[]);
    assert_eq!(
        v["title"].as_str().unwrap(),
        "node",
        "process.title must be \"node\" (was the full binary path before the fix)"
    );
    assert_eq!(
        v["argv0"].as_str().unwrap(),
        "node",
        "process.argv0 must be \"node\""
    );
    assert!(
        v["execPathIsAbsolute"].as_bool().unwrap(),
        "process.execPath must remain an absolute path (must not become \"node\")"
    );
}

/// `--node` compat mode must also report `"node"` — the fix applies to the
/// compat spawn path as well.
#[test]
fn node_compat_mode_reports_node_as_title_and_argv0() {
    let v = run_identity(&["--node"]);
    assert_eq!(
        v["title"].as_str().unwrap(),
        "node",
        "process.title must be \"node\" in --node compat mode"
    );
    assert_eq!(
        v["argv0"].as_str().unwrap(),
        "node",
        "process.argv0 must be \"node\" in --node compat mode"
    );
    assert!(
        v["execPathIsAbsolute"].as_bool().unwrap(),
        "process.execPath must remain an absolute path in --node compat mode"
    );
}
