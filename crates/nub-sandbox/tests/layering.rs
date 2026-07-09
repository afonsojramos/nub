//! Tighten-only layering (`LayeredFs`, `select_terms`) and scope resolution —
//! the seams the future project-config frontend plugs into. The frontend-less
//! `--sandbox` entry is single-term, so these are only reachable here; Phase R
//! covers them so the frontend lands on tested ground.

mod common;

use nub_sandbox::compiler::compile;
use nub_sandbox::compiler::layering::{LayeredFs, select_terms};
use nub_sandbox::compiler::scope::{ScopeCandidate, resolve};
use nub_sandbox::policy::FsPolicy;
use serde_json::{Value, json};

/// Compile a surface `{ fs: … }` block and return just its resolved fs policy.
fn fs_policy(surface: Value) -> FsPolicy {
    let ctx = common::ctx(true, &[]);
    compile(&surface, &ctx).unwrap().fs
}

#[test]
fn layered_fs_is_the_intersection_least_permissive_wins() {
    // Layer A grants rw to two dirs; layer B only to one. The composite may read/
    // write a path only if EVERY layer permits it (tighten-only).
    let a = fs_policy(json!({ "fs": { "./data": "rw", "./shared": "rw" } }));
    let b = fs_policy(json!({ "fs": { "./data": "rw" } }));
    let layered = LayeredFs::new(&[&a, &b]);
    let proj = common::homes().project;

    assert!(
        layered.writable(&proj.join("data/x")),
        "both grant rw → writable"
    );
    assert!(layered.readable(&proj.join("data/x")));
    assert!(
        !layered.writable(&proj.join("shared/x")),
        "only A grants shared → not writable under the intersection"
    );
    assert!(
        !layered.readable(&proj.join("shared/x")),
        "B's deny base makes shared unreadable in the composite"
    );
}

#[test]
fn layered_fs_lower_layer_cannot_widen_a_higher_deny() {
    // A tighten-only ratchet: even though layer A re-allows the ssh subtree, layer
    // B (the floor) denies it, so the composite denies it — a lower-trust layer
    // can only ADD restrictions.
    let wide = fs_policy(json!({ "fs": ["...", "~/.ssh"] })); // re-allows ssh
    let floor = fs_policy(json!({ "fs": ["...", "!~/.ssh"] })); // denies ssh
    let layered = LayeredFs::new(&[&wide, &floor]);
    let ssh = common::homes().home.join(".ssh/id_rsa");
    assert!(!layered.readable(&ssh), "the floor's deny wins");
}

#[test]
fn select_terms_config_override_replaces_project_but_floor_stands() {
    let higher = ["cli".to_string()];
    let floor = "user-global".to_string();
    let project = "project".to_string();
    let cfg = "--config".to_string();

    // No override: CLI + floor + project.
    let terms = select_terms(&higher, Some(&floor), Some(&project), None);
    assert_eq!(
        terms.into_iter().cloned().collect::<Vec<_>>(),
        vec!["cli", "user-global", "project"]
    );

    // --config replaces the project term; the user-global floor still applies.
    let terms = select_terms(&higher, Some(&floor), Some(&project), Some(&cfg));
    assert_eq!(
        terms.into_iter().cloned().collect::<Vec<_>>(),
        vec!["cli", "user-global", "--config"],
        "project is replaced, floor is not pierced"
    );
}

#[test]
fn scope_resolution_picks_the_most_specific_present() {
    let run = json!({ "fs": ["./a"] });
    let script = json!({ "fs": ["./b"] });
    // Candidates are least- to most-specific; the most-specific PRESENT one wins.
    let picked = resolve(&[
        ScopeCandidate {
            label: "run",
            value: Some(&run),
        },
        ScopeCandidate {
            label: "scriptsMeta.dev",
            value: Some(&script),
        },
        ScopeCandidate {
            label: "dependenciesMeta.foo",
            value: None,
        },
    ]);
    assert_eq!(picked.map(|(l, _)| l), Some("scriptsMeta.dev"));

    // Nothing present → None (caller applies its built-in default).
    let none = resolve(&[ScopeCandidate {
        label: "run",
        value: None,
    }]);
    assert!(none.is_none());
}
