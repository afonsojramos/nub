//! Compiler tests: the wrapper trichotomy, preset expansion, per-axis fold, the
//! env type grammar, `$(…)` trust gating, and the error surface.

mod common;

use nub_sandbox::compiler::{CompileError, compile};
use nub_sandbox::policy::{Effect, EnvFormat};
use serde_json::json;

// ── wrapper trichotomy ────────────────────────────────────────────────────────

#[test]
fn false_fully_unjails() {
    let ctx = common::ctx(false, &[("SECRET", "x")]);
    let p = compile(&json!(false), &ctx).unwrap();
    assert!(
        matches!(p.fs.rules.default_effect, Effect::Allow),
        "fs allow-all"
    );
    assert!(!p.net.enforce, "net not enforcing");
    assert!(!p.env.enforce, "env inherited");
}

#[test]
fn true_is_secure_default_per_axis() {
    let ctx = common::ctx(
        true,
        &[("PATH", "/usr/bin"), ("AWS_SECRET_ACCESS_KEY", "sk")],
    );
    let p = compile(&json!(true), &ctx).unwrap();
    assert!(p.net.enforce && p.net.rules.is_empty(), "net deny-all");
    assert!(p.env.enforce, "env constructed");
    assert!(
        p.env.constructed.contains_key("PATH"),
        "baseline keeps PATH"
    );
    assert!(
        !p.env.constructed.contains_key("AWS_SECRET_ACCESS_KEY"),
        "baseline drops secrets"
    );
}

#[test]
fn absent_granular_axis_is_relaxed_not_confined() {
    // A granular object confines what you name: net + env omitted → relaxed.
    let ctx = common::ctx(true, &[("ANYTHING", "1")]);
    let p = compile(&json!({ "fs": ["./data"] }), &ctx).unwrap();
    assert!(
        matches!(p.fs.rules.default_effect, Effect::Deny),
        "fs confined"
    );
    assert!(!p.net.enforce, "net relaxed (absent)");
    assert!(!p.env.enforce, "env relaxed (absent)");
}

// ── presets ───────────────────────────────────────────────────────────────────

#[test]
fn build_jail_preset_expands() {
    let ctx = common::ctx(true, &[("PATH", "/bin"), ("NPM_TOKEN", "t")]);
    let p = compile(&json!("build-jail"), &ctx).unwrap();
    assert!(
        p.net.enforce && p.net.rules.is_empty(),
        "build-jail denies egress"
    );
    assert!(
        p.env.enforce && p.env.constructed.is_empty(),
        "build-jail strips env"
    );
    // project subtree is writable, secret set still denied.
    let m = nub_sandbox::matcher::PathMatcher::new(&p.fs.rules);
    let proj = common::homes().project;
    let d = m.decide(&proj.join("build/out.o"));
    assert!(
        matches!(d.effect, Effect::Allow)
            && matches!(d.access, nub_sandbox::policy::FsAccess::ReadWrite)
    );
}

#[test]
fn unknown_preset_is_a_hard_error_naming_the_set() {
    let ctx = common::ctx(true, &[]);
    let err = compile(&json!("no-such-preset"), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::UnknownPreset { .. }));
    assert!(err.to_string().contains("build-jail"));
}

#[test]
fn path_like_string_is_an_unresolved_file_ref() {
    let ctx = common::ctx(true, &[]);
    let err = compile(&json!("./policy.json"), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::FileRefUnresolved { .. }));
}

// ── unknown keys fail loud ────────────────────────────────────────────────────

#[test]
fn unknown_axis_key_fails() {
    let ctx = common::ctx(true, &[]);
    let err = compile(&json!({ "fs": true, "bogus": 1 }), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::Shape { .. }));
}

// ── env grammar ───────────────────────────────────────────────────────────────

#[test]
fn env_array_allowlist_and_deny_last_match_wins() {
    let ctx = common::ctx(
        true,
        &[
            ("NODE_ENV", "prod"),
            ("VITE_URL", "x"),
            ("API_TOKEN", "secret"),
            ("OTHER", "y"),
        ],
    );
    // allow NODE_ENV + VITE_*, then deny *_TOKEN.
    let p = compile(&json!({ "env": ["NODE_ENV", "VITE_*", "!*_TOKEN"] }), &ctx).unwrap();
    let c = &p.env.constructed;
    assert_eq!(c.get("NODE_ENV").map(String::as_str), Some("prod"));
    assert_eq!(c.get("VITE_URL").map(String::as_str), Some("x"));
    assert!(!c.contains_key("API_TOKEN"), "denied");
    assert!(
        !c.contains_key("OTHER"),
        "not allowlisted → excluded (default-deny)"
    );
    assert!(p.env.withheld.contains(&"OTHER".to_string()));
}

#[test]
fn env_spread_defaults_deny_secrets_but_ordering_can_reallow() {
    let ctx = common::ctx(true, &[("GITHUB_TOKEN", "gh"), ("NORMAL", "n")]);
    // `["*", "..."]` — allow all, then secret defaults deny → GITHUB_TOKEN gone.
    let denied = compile(&json!({ "env": ["*", "..."] }), &ctx).unwrap();
    assert!(!denied.env.constructed.contains_key("GITHUB_TOKEN"));
    assert!(denied.env.constructed.contains_key("NORMAL"));
    // `["...", "*"]` — defaults first, then allow-all wins by ordering.
    let allowed = compile(&json!({ "env": ["...", "*"] }), &ctx).unwrap();
    assert!(
        allowed.env.constructed.contains_key("GITHUB_TOKEN"),
        "later allow wins"
    );
}

#[test]
fn env_object_types_validate() {
    let ctx = common::ctx(true, &[("PORT", "8080"), ("COUNT", "12")]);
    let p = compile(
        &json!({ "env": { "PORT": "port", "COUNT": "integer" } }),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        p.env.constructed.get("PORT").map(String::as_str),
        Some("8080")
    );
    assert!(
        p.env
            .schema
            .iter()
            .any(|r| r.key == "PORT" && r.format == Some(EnvFormat::Port))
    );

    let bad = common::ctx(true, &[("PORT", "notaport")]);
    let err = compile(&json!({ "env": { "PORT": "port" } }), &bad).unwrap_err();
    assert!(matches!(err, CompileError::Validation { .. }));
}

#[test]
fn env_regex_and_literal_union() {
    let ctx = common::ctx(true, &[("MODE", "dev"), ("SHA", "abc123")]);
    let p = compile(
        &json!({ "env": { "MODE": "'dev' | 'prod'", "SHA": "/^[a-f0-9]+$/" } }),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        p.env.constructed.get("MODE").map(String::as_str),
        Some("dev")
    );
    assert_eq!(
        p.env.constructed.get("SHA").map(String::as_str),
        Some("abc123")
    );

    let bad = common::ctx(true, &[("MODE", "staging")]);
    assert!(compile(&json!({ "env": { "MODE": "'dev' | 'prod'" } }), &bad).is_err());
}

#[test]
fn env_unknown_type_names_the_supported_set() {
    let ctx = common::ctx(true, &[("X", "1")]);
    let err = compile(&json!({ "env": { "X": "email" } }), &ctx).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("integer") && msg.contains("port"),
        "names the closed set: {msg}"
    );
}

#[test]
fn env_required_missing_key_errors_optional_is_ok() {
    let ctx = common::ctx(true, &[]);
    // required (no `?`) and absent → error.
    let err = compile(&json!({ "env": { "DATABASE_URL": true } }), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::MissingRequired { .. }));
    // optional (`?`) and absent → fine.
    assert!(compile(&json!({ "env": { "DATABASE_URL?": true } }), &ctx).is_ok());
}

#[test]
fn env_secret_and_public_marks() {
    let ctx = common::ctx(true, &[("PUB", "1"), ("PRIV", "2")]);
    let p = compile(
        &json!({ "env": { "PUB": { "public": true }, "PRIV": { "secret": true } } }),
        &ctx,
    )
    .unwrap();
    let pub_rule = p.env.schema.iter().find(|r| r.key == "PUB").unwrap();
    let priv_rule = p.env.schema.iter().find(|r| r.key == "PRIV").unwrap();
    assert!(!pub_rule.secret, "public opts out of sensitive");
    assert!(priv_rule.secret);
}

// ── $(…) substitution + trust gate ────────────────────────────────────────────

#[test]
fn substitution_resolves_in_trusted_home() {
    let ctx = common::ctx(true, &[]);
    let p = compile(&json!({ "env": { "GREETING": "$(echo hi)" } }), &ctx).unwrap();
    assert_eq!(
        p.env.constructed.get("GREETING").map(String::as_str),
        Some("hi")
    );
}

#[test]
fn substitution_embedded_in_a_larger_value() {
    let ctx = common::ctx(true, &[]);
    let p = compile(
        &json!({ "env": { "URL": { "value": "https://$(echo hi)/path" } } }),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        p.env.constructed.get("URL").map(String::as_str),
        Some("https://hi/path")
    );
}

#[test]
fn substitution_forbidden_in_untrusted_home() {
    let ctx = common::ctx(false, &[]);
    let err = compile(&json!({ "env": { "X": "$(echo hi)" } }), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::UntrustedSubstitution { .. }));
}

#[test]
fn substitution_failure_surfaces() {
    let ctx = common::ctx(true, &[]);
    let err = compile(&json!({ "env": { "X": "$(fail)" } }), &ctx).unwrap_err();
    assert!(matches!(err, CompileError::Substitution { .. }));
}

// ── net fold ──────────────────────────────────────────────────────────────────

#[test]
fn net_array_hosts_and_cidr_classify() {
    let ctx = common::ctx(true, &[]);
    let p = compile(
        &json!({ "net": ["*.sentry.io", "10.0.0.0/8", "!evil.com"] }),
        &ctx,
    )
    .unwrap();
    assert!(p.net.enforce);
    let m = nub_sandbox::matcher::HostMatcher::new(&p.net);
    assert!(m.admits("in.sentry.io"));
    assert!(m.admits("10.2.3.4"));
    assert!(!m.admits("evil.com"));
    assert!(!m.admits("unlisted.com"));
}

#[test]
fn net_true_disables_enforcement_false_denies_all() {
    let ctx = common::ctx(true, &[]);
    assert!(!compile(&json!({ "net": true }), &ctx).unwrap().net.enforce);
    let denied = compile(&json!({ "net": false }), &ctx).unwrap();
    assert!(denied.net.enforce && denied.net.rules.is_empty());
}
