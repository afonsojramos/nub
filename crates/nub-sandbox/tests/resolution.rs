//! U1 resolution-model tests: the clobber warning, object-level `"..."`, and
//! cross-scope `"..."` inheritance driven against SYNTHETIC scope chains (the
//! project frontend that feeds real multi-scope configs does not exist yet — this
//! locks the resolution LOGIC).

mod common;

use nub_sandbox::compiler::compile;
use nub_sandbox::compiler::compile_with_warnings;
use nub_sandbox::compiler::scope::{ChainScope, resolve_chain};
use nub_sandbox::matcher::{HostMatcher, PathMatcher};
use nub_sandbox::policy::{Effect, FsAccess};
use serde_json::{Value, json};

fn warns(surface: Value) -> bool {
    let ctx = common::ctx(true, &[]);
    !compile_with_warnings(&surface, &ctx).unwrap().1.is_empty()
}

// ── clobber warning (D2b/D6) ───────────────────────────────────────────────────

#[test]
fn total_shadow_warns() {
    // A later entry whose match-set covers an earlier one → the earlier is dead.
    assert!(
        warns(json!({ "fs": ["./data", "!./data"] })),
        "X then !X (equal set)"
    );
    assert!(warns(json!({ "fs": ["./data", "./data"] })), "duplicate");
    assert!(
        warns(json!({ "fs": ["!./data/secret", "./data"] })),
        "narrow deny then broad allow covering it (order-matters footgun)"
    );
    assert!(
        warns(json!({ "fs": ["./a", "**"] })),
        "later ** covers everything"
    );
    assert!(
        warns(json!({ "net": ["in.sentry.io", "*.sentry.io"] })),
        "host then covering wildcard"
    );
    assert!(
        warns(json!({ "net": ["a.com", "*"] })),
        "later * covers all hosts"
    );
    assert!(
        warns(json!({ "env": ["VITE_URL", "VITE_*"] })),
        "key then covering prefix glob"
    );
    assert!(warns(json!({ "env": ["FOO", "FOO"] })), "duplicate env key");
    assert!(
        warns(json!({ "env": ["NODE_ENV", "*"] })),
        "later * covers all keys"
    );
}

#[test]
fn partial_override_and_sentinels_stay_silent() {
    // The intended granular idiom (allow a block, deny one key inside it) is NOT a
    // shadow — the earlier entry still decides for the rest of its set.
    assert!(
        !warns(json!({ "fs": ["./data", "!./data/secret"] })),
        "broad allow, narrow deny"
    );
    assert!(
        !warns(json!({ "env": ["*", "!*_TOKEN"] })),
        "allow-all then strip a class"
    );
    assert!(
        !warns(json!({ "net": ["*", "!*.evil.com"] })),
        "best-effort denylist"
    );
    assert!(!warns(json!({ "fs": ["./a", "./b"] })), "disjoint paths");
    assert!(
        !warns(json!({ "env": ["FOO", "BAR", "!*_TOKEN"] })),
        "disjoint keys + class deny"
    );
    // `"..."` / `"!..."` are excluded from clobber analysis (they expand to many
    // rules); the canonical `["...", specific]` idiom never warns.
    assert!(
        !warns(json!({ "fs": ["...", "./data"] })),
        "inherit then specific"
    );
    assert!(
        !warns(json!({ "env": ["*", "..."] })),
        "allow-all then inherit"
    );
    // A three-entry partial (allow block, deny sub, re-allow sub-sub) stays silent.
    assert!(
        !warns(json!({ "fs": ["./data", "!./data/secret", "./data/secret/pub"] })),
        "nested partial overrides"
    );
}

// ── object-level `"..."` (inherit base for unlisted axes) ──────────────────────

#[test]
fn object_spread_at_outermost_equals_sandbox_true() {
    // `{ "...": true }` at the outermost scope resolves to the built-in base for
    // every unlisted axis → exactly `sandbox: true`.
    let ctx = common::ctx(true, &[("PATH", "/bin"), ("SECRET_TOKEN", "s")]);
    let spread = compile(&json!({ "...": true }), &ctx).unwrap();
    let truth = compile(&json!(true), &ctx).unwrap();
    assert_eq!(spread, truth, "{{ \"...\": true }} ≡ sandbox: true");
}

#[test]
fn object_spread_keeps_unlisted_axes_from_flooring() {
    // With object-level `"..."`, an unlisted axis inherits the base instead of
    // flooring: `{ "...": true, "fs": ["./x"] }` keeps the secure net/env while
    // confining fs — the §4.3 "keep the project's net/env, adjust fs" idiom.
    let ctx = common::ctx(true, &[("PATH", "/bin"), ("MY_TOKEN", "leak")]);
    let p = compile(&json!({ "...": true, "fs": ["./x"] }), &ctx).unwrap();
    assert!(
        p.net.enforce && p.net.rules.is_empty(),
        "net inherited base (deny-all)"
    );
    assert!(
        p.env.constructed.contains_key("PATH"),
        "env inherited curated baseline"
    );
    assert!(
        !p.env.constructed.contains_key("MY_TOKEN"),
        "baseline still drops the secret"
    );
    // Contrast: WITHOUT the object spread, net + env would floor to strip-all.
    let floored = compile(&json!({ "fs": ["./x"] }), &ctx).unwrap();
    assert!(floored.env.constructed.is_empty(), "no spread → env floors");
}

// ── cross-scope `"..."` inheritance (synthetic chains) ─────────────────────────

fn chain(parent: &Value, child: &Value) -> nub_sandbox::policy::SandboxPolicy {
    let ctx = common::ctx(true, &[("FOO", "1"), ("BAR", "2"), ("PATH", "/bin")]);
    resolve_chain(
        &[
            ChainScope {
                label: "project",
                surface: Some(parent),
            },
            ChainScope {
                label: "script",
                surface: Some(child),
            },
        ],
        &ctx,
    )
    .unwrap()
    .0
}

#[test]
fn fs_spread_inherits_the_resolved_parent() {
    let parent = json!({ "fs": { "./a": "rw" } });
    // Child inherits parent fs via `"..."`, then adds ./b.
    let p = chain(&parent, &json!({ "fs": ["...", "./b"] }));
    let m = PathMatcher::new(&p.fs.rules);
    let proj = common::homes().project;
    assert!(rw(&m, &proj.join("a/x")), "inherited parent grant ./a");
    assert!(rw(&m, &proj.join("b/x")), "own grant ./b");
    // Child WITHOUT `"..."` floors: only ./b, parent's ./a NOT inherited.
    let f = chain(&parent, &json!({ "fs": ["./b"] }));
    let mf = PathMatcher::new(&f.fs.rules);
    assert!(rw(&mf, &proj.join("b/x")), "own grant ./b");
    assert!(
        matches!(mf.decide(&proj.join("a/x")).effect, Effect::Deny),
        "parent ./a is NOT inherited without an explicit `...`"
    );
}

#[test]
fn net_spread_inherits_the_resolved_parent() {
    let parent = json!({ "net": ["a.com"] });
    let p = chain(&parent, &json!({ "net": ["...", "b.com"] }));
    let m = HostMatcher::new(&p.net);
    assert!(m.admits("a.com") && m.admits("b.com"), "inherit + extend");
    // No `"..."` → floor: only b.com.
    let f = chain(&parent, &json!({ "net": ["b.com"] }));
    let mf = HostMatcher::new(&f.net);
    assert!(
        mf.admits("b.com") && !mf.admits("a.com"),
        "parent host NOT inherited"
    );
}

#[test]
fn env_spread_inherits_the_resolved_parent() {
    let parent = json!({ "env": { "FOO": true } }); // parent env = {FOO}
    // Object-form `"..."` key inherits the parent env, then adds BAR.
    let p = chain(&parent, &json!({ "env": { "...": true, "BAR": true } }));
    assert_eq!(
        p.env.constructed.get("FOO").map(String::as_str),
        Some("1"),
        "inherited FOO"
    );
    assert_eq!(
        p.env.constructed.get("BAR").map(String::as_str),
        Some("2"),
        "own BAR"
    );
    assert!(
        !p.env.constructed.contains_key("PATH"),
        "ambient PATH not pulled in"
    );
    // No `"..."` → floor: BAR only, parent FOO NOT inherited.
    let f = chain(&parent, &json!({ "env": { "BAR": true } }));
    assert!(
        f.env.constructed.contains_key("BAR") && !f.env.constructed.contains_key("FOO"),
        "FOO not inherited"
    );
    // A child deny after inherit removes an inherited key (last-match).
    let d = chain(&parent, &json!({ "env": { "...": true, "FOO": false } }));
    assert!(
        !d.env.constructed.contains_key("FOO"),
        "child deny removes inherited FOO"
    );
}

#[test]
fn keyless_scope_cascades_the_parent_whole_policy() {
    // A scope with NO `sandbox` key inherits its parent's WHOLE policy (cascade —
    // it can't escape confinement by saying nothing).
    let ctx = common::ctx(true, &[("FOO", "1")]);
    let parent = json!({ "fs": { "./a": "rw" }, "net": ["a.com"], "env": { "FOO": true } });
    let (p, _) = resolve_chain(
        &[
            ChainScope {
                label: "project",
                surface: Some(&parent),
            },
            ChainScope {
                label: "script",
                surface: None,
            }, // keyless
        ],
        &ctx,
    )
    .unwrap();
    let parent_policy = compile(&parent, &ctx).unwrap();
    assert_eq!(p, parent_policy, "keyless child == parent (cascade)");
}

#[test]
fn object_spread_inner_inherits_parent_for_unlisted_axes() {
    // `{ "...": true, "fs": ["...", "./b"] }` at an inner scope: inherit parent
    // net+env, and inherit-then-extend parent fs.
    let parent = json!({ "fs": { "./a": "rw" }, "net": ["a.com"], "env": { "FOO": true } });
    let p = chain(&parent, &json!({ "...": true, "fs": ["...", "./b"] }));
    let m = PathMatcher::new(&p.fs.rules);
    let proj = common::homes().project;
    assert!(
        rw(&m, &proj.join("a/x")) && rw(&m, &proj.join("b/x")),
        "fs inherit + extend"
    );
    assert!(HostMatcher::new(&p.net).admits("a.com"), "net inherited");
    assert!(p.env.constructed.contains_key("FOO"), "env inherited");
}

fn rw(m: &PathMatcher, path: &std::path::Path) -> bool {
    let d = m.decide(path);
    matches!(d.effect, Effect::Allow) && matches!(d.access, FsAccess::ReadWrite)
}
