//! Matcher tests: symbolic-root expansion, canonicalize-including-nonexistent,
//! host-glob apex/subdomain semantics, and CIDR dispatch.

mod common;

use nub_sandbox::matcher::host::host_glob_matches;
use nub_sandbox::matcher::path::{canonicalize_including_nonexistent, expand_symbolic};
use nub_sandbox::matcher::{HostMatcher, PathMatcher};
use nub_sandbox::policy::{Effect, FsAccess, NetPolicy, NetRule, NetTarget};
use std::path::PathBuf;

// ── symbolic expansion ────────────────────────────────────────────────────────

#[test]
fn expands_home_and_symbolic_roots() {
    let h = common::homes();
    assert_eq!(
        expand_symbolic("~/.ssh", &h),
        format!("{}/.ssh", h.home.display())
    );
    assert_eq!(expand_symbolic("~", &h), h.home.to_string_lossy());
    assert_eq!(
        expand_symbolic("<tmp>/x", &h),
        format!("{}/x", h.tmp.display())
    );
    assert_eq!(
        expand_symbolic("<cache>/y", &h),
        format!("{}/y", h.cache.display())
    );
}

#[test]
fn expands_bare_relative_under_project() {
    let h = common::homes();
    assert_eq!(
        expand_symbolic("./data", &h),
        format!("{}/data", h.project.display())
    );
    assert_eq!(
        expand_symbolic("data/**", &h),
        format!("{}/data/**", h.project.display())
    );
}

#[test]
fn absolute_paths_pass_through_slash_normalized() {
    let h = common::homes();
    // Backslashes normalize to forward slashes even in an absolute literal.
    assert_eq!(expand_symbolic("/etc/hosts", &h), "/etc/hosts");
    assert_eq!(expand_symbolic("/a\\b", &h), "/a/b");
}

// ── canonicalize including non-existent ───────────────────────────────────────

#[test]
fn canonicalizes_nonexistent_tail_without_erroring() {
    // The disavowed-backend trap: canonicalize must NOT Err on a path whose tail
    // does not exist — it resolves the existing prefix and appends the rest.
    let tmp = std::env::temp_dir();
    let target = tmp.join("nub-sbx-does-not-exist-xyz").join("child");
    let canon = canonicalize_including_nonexistent(&target);
    // The existing prefix (temp_dir) is resolved (e.g. /tmp→/private/tmp on mac);
    // the non-existent tail is preserved.
    assert!(canon.ends_with("nub-sbx-does-not-exist-xyz/child"));
    assert!(canon.is_absolute());
}

#[test]
fn canonicalize_collapses_parent_dir_in_nonexistent_tail() {
    let tmp = std::env::temp_dir();
    let target = tmp.join("nub-sbx-a").join("..").join("nub-sbx-b");
    let canon = canonicalize_including_nonexistent(&target);
    assert!(canon.ends_with("nub-sbx-b"));
    assert!(!canon.to_string_lossy().contains(".."));
}

// ── fs last-match-wins ────────────────────────────────────────────────────────

#[test]
fn path_matcher_last_match_wins_over_default() {
    use nub_sandbox::compiler::compile;
    use serde_json::json;
    // `["...", "!~/.ssh", "~/.ssh/config"]`: generous read, deny the ssh subtree,
    // then re-allow one file — the LAST match wins, so config is readable.
    let ctx = common::ctx(true, &[]);
    let policy = compile(&json!({ "fs": ["...", "!~/.ssh", "~/.ssh/config"] }), &ctx).unwrap();
    let m = PathMatcher::new(&policy.fs.rules);

    let home = common::homes().home;
    let readable = |p: PathBuf| matches!(m.decide(&p).effect, Effect::Allow);
    assert!(
        readable(home.join("notes.txt")),
        "generous read allows a normal file"
    );
    assert!(!readable(home.join(".ssh/id_rsa")), "ssh subtree denied");
    assert!(
        readable(home.join(".ssh/config")),
        "later specific allow wins"
    );
}

#[test]
fn fs_rw_grant_is_writable_read_only_grant_is_not() {
    use nub_sandbox::compiler::compile;
    use serde_json::json;
    let ctx = common::ctx(true, &[]);
    let policy = compile(&json!({ "fs": { "./rw": "rw", "./ro": "r" } }), &ctx).unwrap();
    let m = PathMatcher::new(&policy.fs.rules);
    let proj = common::homes().project;

    let d_rw = m.decide(&proj.join("rw/file"));
    assert!(matches!(d_rw.effect, Effect::Allow) && matches!(d_rw.access, FsAccess::ReadWrite));
    let d_ro = m.decide(&proj.join("ro/file"));
    assert!(matches!(d_ro.effect, Effect::Allow) && matches!(d_ro.access, FsAccess::Read));
}

#[test]
fn deny_is_not_dodged_by_parent_dir_traversal() {
    use nub_sandbox::compiler::compile;
    use serde_json::json;
    // A `..` bounce back into a denied subtree must still hit the deny — the
    // candidate is canonicalized (incl. non-existent tail) before matching.
    let ctx = common::ctx(true, &[]);
    let policy = compile(&json!({ "fs": ["...", "!~/.ssh"] }), &ctx).unwrap();
    let m = PathMatcher::new(&policy.fs.rules);
    let dodge = common::homes().home.join(".ssh/../.ssh/id_rsa");
    assert!(
        matches!(m.decide(&dodge).effect, Effect::Deny),
        "`..` traversal must not dodge the ssh deny"
    );
}

// ── host glob + CIDR ──────────────────────────────────────────────────────────

#[test]
fn host_wildcard_matches_apex_and_any_depth() {
    assert!(host_glob_matches("*.example.com", "example.com"), "apex");
    assert!(
        host_glob_matches("*.example.com", "api.example.com"),
        "one label"
    );
    assert!(
        host_glob_matches("*.example.com", "a.b.example.com"),
        "any depth"
    );
    assert!(!host_glob_matches("*.example.com", "example.org"));
    assert!(!host_glob_matches("*.example.com", "notexample.com"));
    assert!(host_glob_matches("*", "anything.at.all"));
}

#[test]
fn host_literal_is_exact_case_insensitive() {
    assert!(host_glob_matches("Example.COM", "example.com"));
    assert!(!host_glob_matches("example.com", "api.example.com"));
}

#[test]
fn net_matcher_admits_by_last_match_and_cidr() {
    let policy = NetPolicy {
        enforce: true,
        default_effect: Effect::Deny,
        rules: vec![
            NetRule {
                target: NetTarget::Host("*.sentry.io".into()),
                effect: Effect::Allow,
            },
            NetRule {
                target: NetTarget::Cidr("10.0.0.0/8".parse().unwrap()),
                effect: Effect::Allow,
            },
        ],
    };
    let m = HostMatcher::new(&policy);
    assert!(m.admits("ingest.sentry.io"));
    assert!(m.admits("10.1.2.3"), "IP in CIDR");
    assert!(!m.admits("evil.com"), "deny-all base");
    assert!(!m.admits("192.168.1.1"), "IP outside CIDR");
}

#[test]
fn net_not_enforcing_admits_everything() {
    let policy = NetPolicy {
        enforce: false,
        ..Default::default()
    };
    let m = HostMatcher::new(&policy);
    assert!(m.admits("anything.com"));
}

// ── secret defaults deny .env at any depth ────────────────────────────────────

#[test]
fn generous_read_still_denies_dotenv_and_ssh() {
    use nub_sandbox::compiler::compile;
    use serde_json::json;
    let ctx = common::ctx(true, &[]);
    let policy = compile(&json!(true), &ctx).unwrap(); // secure defaults
    let m = PathMatcher::new(&policy.fs.rules);
    let home = common::homes().home;
    let proj = common::homes().project;

    assert!(matches!(
        m.decide(&proj.join("src/index.ts")).effect,
        Effect::Allow
    ));
    assert!(
        matches!(m.decide(&proj.join(".env")).effect, Effect::Deny),
        ".env denied"
    );
    assert!(
        matches!(
            m.decide(&proj.join("packages/app/.env.local")).effect,
            Effect::Deny
        ),
        "nested .env denied"
    );
    assert!(
        matches!(m.decide(&home.join(".ssh/id_rsa")).effect, Effect::Deny),
        "ssh denied"
    );
}
