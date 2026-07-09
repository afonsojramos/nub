//! IR serde round-trip + the conformance harness (compiler/matcher verdicts vs a
//! fixture manifest) + the apply() env-scrub skeleton.

mod common;

use nub_sandbox::compiler::compile;
use nub_sandbox::conformance::{Fixture, run_fixture};
use nub_sandbox::policy::SandboxPolicy;
use nub_sandbox::{CommandSpec, apply};
use serde_json::json;

#[test]
fn policy_round_trips_through_serde() {
    let ctx = common::ctx(true, &[("PORT", "3000"), ("NODE_ENV", "prod")]);
    let policy = compile(
        &json!({
            "fs": ["...", "!~/.ssh", "./data"],
            "net": ["*.sentry.io", "10.0.0.0/8"],
            "env": { "PORT": "port", "NODE_ENV": true }
        }),
        &ctx,
    )
    .unwrap();

    let text = serde_json::to_string(&policy).unwrap();
    let back: SandboxPolicy = serde_json::from_str(&text).unwrap();
    assert_eq!(
        policy, back,
        "IR must round-trip byte-for-byte through serde"
    );
}

#[test]
fn conformance_fixture_passes_when_verdicts_match() {
    let ctx = common::ctx(true, &[("KEEP", "1"), ("DROP_TOKEN", "s")]);
    let proj = common::homes().project;
    let fixture: Fixture = serde_json::from_value(json!({
        "name": "basic",
        "sandbox": {
            "fs": ["...", "./build"],
            "net": ["github.com"],
            "env": ["KEEP", "!*_TOKEN"]
        },
        "fs": [
            { "path": format!("{}/build/out", proj.display()), "read": true, "write": true },
            { "path": format!("{}/.env", proj.display()), "read": false, "write": false }
        ],
        "net": [
            { "host": "github.com", "admit": true },
            { "host": "evil.com", "admit": false }
        ],
        "env": [
            { "key": "KEEP", "present": true, "value": "1" },
            { "key": "DROP_TOKEN", "present": false }
        ]
    }))
    .unwrap();

    let mismatches = run_fixture(&fixture, &ctx);
    assert!(
        mismatches.is_empty(),
        "fixture should pass, got: {mismatches:?}"
    );
}

#[test]
fn conformance_reports_a_mismatch() {
    let ctx = common::ctx(true, &[]);
    let fixture: Fixture = serde_json::from_value(json!({
        "name": "wrong-expectation",
        "sandbox": { "net": false },   // deny all egress
        "net": [ { "host": "github.com", "admit": true } ] // wrong: it's denied
    }))
    .unwrap();
    let mismatches = run_fixture(&fixture, &ctx);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].axis, "net");
}

#[test]
fn apply_scrubs_env_when_enforced() {
    let ctx = common::ctx(true, &[("KEEP", "1"), ("SECRET_TOKEN", "s")]);
    let policy = compile(&json!({ "env": ["KEEP"] }), &ctx).unwrap();
    let prepared = apply(&policy, CommandSpec::new("true")).unwrap();
    // The env axis is enforced by construction; fs/net report as not-yet-enforced.
    let envs: std::collections::BTreeMap<_, _> = prepared
        .command
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    assert_eq!(envs.get("KEEP").map(String::as_str), Some("1"));
    assert!(
        !envs.contains_key("SECRET_TOKEN"),
        "secret withheld from child env"
    );
}

#[test]
fn apply_degradation_reflects_backend_capability() {
    let ctx = common::ctx(true, &[]);
    let policy = compile(&json!({ "fs": ["./x"], "net": false }), &ctx).unwrap();
    let prepared = apply(&policy, CommandSpec::new("true")).unwrap();
    let d = &prepared.degradation;
    // macOS has a real Seatbelt backend: fs + deny-all net are genuinely enforced,
    // so nothing is degraded. Other OSes still run the env-scrub skeleton, which
    // honestly reports fs + net as not-enforced.
    #[cfg(target_os = "macos")]
    assert!(d.is_full(), "macOS enforces fs + deny-all net");
    #[cfg(not(target_os = "macos"))]
    {
        assert!(!d.is_full(), "skeleton does not enforce fs/net");
        assert!(d.lost.contains(&"fs".to_string()));
        assert!(d.lost.contains(&"net".to_string()));
    }
}
