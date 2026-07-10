//! Per-axis fold: an axis surface value (`false | true | array | object`) →
//! its resolved IR fragment. The `"..."` spread and last-match-wins ORDER are
//! discharged here into a flat ordered list; the actual last-match decision is
//! made at evaluation time by the matcher, so the fold only has to preserve
//! order and splice the defaults at the sentinel's position.

use super::defaults;
use super::env_grammar::{EnvType, parse_env_type};
use super::resolve;
use super::{CompileCtx, CompileError};
use crate::matcher::path::expand_symbolic;
use crate::policy::{
    CanonGlob, Effect, EnvFormat, EnvPolicy, EnvRule, FsAccess, FsPolicy, FsRule, FsRuleSet,
    NetPolicy, NetRule, NetTarget,
};
use globset::{GlobBuilder, GlobMatcher};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A `"!..."` entry — a negated inheritance sentinel — is meaningless (you cannot
/// deny "the inherited scope") and is a shape error on every axis (D-list).
const SENTINEL_NEGATE_MSG: &str =
    "`!...` is invalid — `\"...\"` is the inheritance sentinel and cannot be negated";
/// An empty / whitespace-only fs entry used to expand to `**` (a silent whole-fs
/// grant, fail-OPEN); it is now a hard shape error (D3).
const EMPTY_FS_ENTRY_MSG: &str = "an empty fs entry is not allowed (it would grant the whole filesystem) — name a path or remove it";
/// `"..."` inheritance in fs/net is positional in the ARRAY form; as an OBJECT key
/// it has no defined meaning, so it is rejected rather than silently treated as a
/// literal path/host named `...` (fail loud, parity with env-object + the array).
const OBJECT_SENTINEL_MSG: &str = "`\"...\"` inheritance is only valid in fs/net array form (e.g. [\"...\", …]), not as an object key";

// ── fs ───────────────────────────────────────────────────────────────────────

/// Fold the `fs` axis value into an [`FsPolicy`]. Array entries and object keys
/// are subtree-expanded (a bare path grants the node + `/**`); a glob-bearing
/// pattern is emitted verbatim. Access: array grants are ReadWrite (the concise
/// "these paths are fully usable" form); object values pick `"r"`/`"rw"`. A
/// `"..."` inherits the enclosing scope's fs at its position: the resolved
/// `parent` when present (cross-scope inheritance), else the built-in generous-
/// read + secret-deny base (outermost scope).
pub fn fold_fs(
    value: &Value,
    ctx: &CompileCtx,
    path: &str,
    parent: Option<&FsPolicy>,
) -> Result<FsPolicy, CompileError> {
    let mut set = FsRuleSet {
        entries: Vec::new(),
        default_effect: Effect::Deny,
    };
    match value {
        // `true` fully relaxes the axis; `false` fully denies it.
        Value::Bool(true) => set.default_effect = Effect::Allow,
        Value::Bool(false) => set.default_effect = Effect::Deny,
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let p = child(path, &i.to_string());
                let s = as_str(item, &p)?;
                fold_fs_array_entry(s, ctx, parent, &p, &mut set.entries)?;
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                fold_fs_object_entry(key, val, ctx, &child(path, key), &mut set.entries)?;
            }
        }
        _ => {
            return Err(CompileError::shape(
                path,
                "fs must be a boolean, an array, or a pattern-keyed object",
            ));
        }
    }
    Ok(FsPolicy {
        rules: set,
        tmp: Default::default(),
    })
}

fn fold_fs_array_entry(
    s: &str,
    ctx: &CompileCtx,
    parent: Option<&FsPolicy>,
    path: &str,
    out: &mut Vec<FsRule>,
) -> Result<(), CompileError> {
    if s == "!..." {
        return Err(CompileError::shape(path, SENTINEL_NEGATE_MSG));
    }
    if s == "..." {
        splice_fs_inherit(ctx, parent, out);
        return Ok(());
    }
    if s.trim().is_empty() {
        return Err(CompileError::shape(path, EMPTY_FS_ENTRY_MSG));
    }
    let (pattern, effect) = match s.strip_prefix('!') {
        Some(rest) => (rest, Effect::Deny),
        None => (s, Effect::Allow),
    };
    // Array grants are ReadWrite; denies deny both.
    push_fs_rules(pattern, effect, FsAccess::ReadWrite, ctx, out);
    Ok(())
}

fn fold_fs_object_entry(
    key: &str,
    val: &Value,
    ctx: &CompileCtx,
    path: &str,
    out: &mut Vec<FsRule>,
) -> Result<(), CompileError> {
    if key == "!..." {
        return Err(CompileError::shape(path, SENTINEL_NEGATE_MSG));
    }
    if key == "..." {
        return Err(CompileError::shape(path, OBJECT_SENTINEL_MSG));
    }
    if key.trim().is_empty() {
        return Err(CompileError::shape(path, EMPTY_FS_ENTRY_MSG));
    }
    let (effect, access) = match val {
        Value::Bool(true) => (Effect::Allow, FsAccess::ReadWrite),
        Value::Bool(false) => (Effect::Deny, FsAccess::Read),
        Value::String(s) => match s.as_str() {
            "r" => (Effect::Allow, FsAccess::Read),
            "rw" => (Effect::Allow, FsAccess::ReadWrite),
            other => {
                return Err(CompileError::shape(
                    path,
                    &format!("fs value `{other}` — expected \"r\", \"rw\", true, or false"),
                ));
            }
        },
        _ => {
            return Err(CompileError::shape(
                path,
                "fs value must be \"r\", \"rw\", true, or false",
            ));
        }
    };
    push_fs_rules(key, effect, access, ctx, out);
    Ok(())
}

/// Expand a surface fs pattern into its canonical subtree globs and push a rule
/// per glob (so `~/.ssh` covers both `~/.ssh` and `~/.ssh/**`).
fn push_fs_rules(
    pattern: &str,
    effect: Effect,
    access: FsAccess,
    ctx: &CompileCtx,
    out: &mut Vec<FsRule>,
) {
    let expanded = expand_symbolic(pattern, &ctx.homes);
    for g in defaults::subtree_globs(&expanded) {
        out.push(FsRule {
            matcher: CanonGlob(crate::matcher::canonicalize_glob_prefix(&g)),
            effect,
            access,
        });
    }
}

/// The fs `"..."` payload: at an inner scope splice the resolved parent's fs
/// entries (cross-scope inheritance); at the outermost scope (no parent) splice
/// the built-in generous-read + secret-deny base — the degenerate outermost case.
fn splice_fs_inherit(ctx: &CompileCtx, parent: Option<&FsPolicy>, out: &mut Vec<FsRule>) {
    match parent {
        Some(p) => out.extend(p.rules.entries.iter().cloned()),
        None => splice_fs_defaults(ctx, out),
    }
}

/// Splice the generous-read base + secret-deny defaults (the built-in fs base).
fn splice_fs_defaults(ctx: &CompileCtx, out: &mut Vec<FsRule>) {
    out.push(defaults::generous_read_allow());
    out.extend(defaults::secret_read_denies(&ctx.homes));
}

// ── net ──────────────────────────────────────────────────────────────────────

/// Fold the `net` axis into a [`NetPolicy`]. Entries are host globs or CIDRs;
/// `!` denies; `"..."` inherits the enclosing scope's net (the resolved `parent`
/// when present; nothing at the outermost scope — the built-in net base is
/// deny-all with no committed allowlist). `net: true` disables enforcement;
/// `net: false` denies all egress.
pub fn fold_net(
    value: &Value,
    path: &str,
    parent: Option<&NetPolicy>,
) -> Result<NetPolicy, CompileError> {
    let mut policy = NetPolicy {
        enforce: true,
        rules: Vec::new(),
        default_effect: Effect::Deny,
    };
    match value {
        Value::Bool(true) => policy.enforce = false,
        Value::Bool(false) => {} // enforce, deny-all base, no rules
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let p = child(path, &i.to_string());
                let s = as_str(item, &p)?;
                fold_net_entry(s, parent, &p, &mut policy.rules)?;
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                let p = child(path, key);
                if key == "!..." {
                    return Err(CompileError::shape(&p, SENTINEL_NEGATE_MSG));
                }
                if key == "..." {
                    return Err(CompileError::shape(&p, OBJECT_SENTINEL_MSG));
                }
                let effect = net_value_effect(val, &p)?;
                push_net_rule(key, effect, &p, &mut policy.rules)?;
            }
        }
        _ => {
            return Err(CompileError::shape(
                path,
                "net must be a boolean, an array, or a pattern-keyed object",
            ));
        }
    }
    Ok(policy)
}

fn fold_net_entry(
    s: &str,
    parent: Option<&NetPolicy>,
    path: &str,
    out: &mut Vec<NetRule>,
) -> Result<(), CompileError> {
    if s == "!..." {
        return Err(CompileError::shape(path, SENTINEL_NEGATE_MSG));
    }
    if s == "..." {
        // Inner scope: inherit the resolved parent's rules. Outermost (no parent):
        // the built-in net base is deny-all with no committed allowlist (the
        // build-jail baseline owns trusted-host allows), so splice nothing.
        if let Some(p) = parent {
            out.extend(p.rules.iter().cloned());
        }
        return Ok(());
    }
    let (pattern, effect) = match s.strip_prefix('!') {
        Some(rest) => (rest, Effect::Deny),
        None => (s, Effect::Allow),
    };
    push_net_rule(pattern, effect, path, out)
}

fn net_value_effect(val: &Value, path: &str) -> Result<Effect, CompileError> {
    match val {
        Value::Bool(true) => Ok(Effect::Allow),
        Value::Bool(false) => Ok(Effect::Deny),
        _ => Err(CompileError::shape(
            path,
            "net value must be true or false (per-host options are not yet supported)",
        )),
    }
}

/// Classify a net target as a CIDR (contains `/` and parses as one) or a host
/// pattern, and push the rule.
fn push_net_rule(
    target: &str,
    effect: Effect,
    path: &str,
    out: &mut Vec<NetRule>,
) -> Result<(), CompileError> {
    let net_target = if target.contains('/') {
        match target.parse::<ipnet::IpNet>() {
            Ok(net) => NetTarget::Cidr(net),
            Err(e) => {
                return Err(CompileError::shape(
                    path,
                    &format!("`{target}` looks like a CIDR but did not parse: {e}"),
                ));
            }
        }
    } else {
        NetTarget::Host(target.to_string())
    };
    out.push(NetRule {
        target: net_target,
        effect,
    });
    Ok(())
}

// ── env ──────────────────────────────────────────────────────────────────────

/// Fold the `env` axis into an [`EnvPolicy`], building the actual child env map.
/// Base is default-DENY (env is constructed, not inherited): a key survives only
/// if the LAST matching entry allows it. `true` passes the whole ambient env;
/// `false` strips everything.
pub fn fold_env(
    value: &Value,
    ctx: &CompileCtx,
    path: &str,
    parent: Option<&EnvPolicy>,
) -> Result<EnvPolicy, CompileError> {
    // An explicit env axis always enforces (constructs the child env exactly).
    let mut policy = EnvPolicy {
        enforce: true,
        ..Default::default()
    };
    match value {
        Value::Bool(true) => {
            policy.constructed = ctx.ambient_env.clone();
            return Ok(policy);
        }
        Value::Bool(false) => {
            policy.withheld = ctx.ambient_env.keys().cloned().collect();
            return Ok(policy);
        }
        Value::Array(items) => {
            let entries = parse_env_array(items, parent, path)?;
            construct_env(&entries, ctx, parent, &mut policy)?;
        }
        Value::Object(map) => {
            let entries = parse_env_object(map, ctx, parent, path)?;
            construct_env(&entries, ctx, parent, &mut policy)?;
        }
        _ => {
            return Err(CompileError::shape(
                path,
                "env must be a boolean, an array, or a pattern-keyed object",
            ));
        }
    }
    Ok(policy)
}

/// One parsed env entry, in surface order.
struct EnvEntry {
    /// The key or glob key the entry governs.
    pattern: String,
    action: EnvAction,
    secret: bool,
    optional: bool,
    format: Option<EnvFormat>,
    /// How `pattern` matches an ambient key. User patterns are case-sensitive
    /// globs; the built-in secret defaults are case-insensitive (glob or
    /// boundary-token) so an uppercase `MY_TOKEN` cannot slip past them.
    key_match: KeyMatch,
    /// A compiler-spliced default entry (the `"..."` curated baseline / inherited
    /// keys / secret denies), NOT user-authored: excluded from the emitted
    /// `schema` (which carries user validation + redaction marks only).
    builtin: bool,
}

/// How an [`EnvEntry`]'s pattern is matched against an ambient env key.
#[derive(Clone, Copy)]
enum KeyMatch {
    /// A user-authored glob/exact key, matched case-SENSITIVELY (POSIX env keys
    /// are case-sensitive; an explicit rule means exactly what it says).
    User,
    /// A built-in secret-KEY guard (`AWS_*`, `NPM_TOKEN`), matched as a
    /// case-INsensitive glob.
    SecretGlob,
    /// A built-in unambiguous secret token (`token`, `credential`), matched
    /// case-insensitively as a SUBSTRING (via `defaults::word_in_substr`).
    SecretSubstr,
    /// A built-in short/ambiguous secret token (`pat`, `pwd`, `auth`), matched
    /// case-insensitively as a whole SEGMENT (via `defaults::word_is_segment`).
    SecretSegment,
    /// The built-in curated baseline (the env `"..."` payload at the OUTERMOST
    /// scope): matches a key iff `defaults::baseline_allows` admits it. One such
    /// allow entry reproduces `sandbox: true`'s curated env exactly.
    CuratedBaseline,
    /// Cross-scope inheritance (the env `"..."` payload at an INNER scope):
    /// matches a key iff it is in the resolved parent's constructed env.
    InheritedKeys,
}

enum EnvAction {
    /// Pass the ambient value through; validate against the type if present.
    Allow(Option<EnvType>),
    /// Construct the key out of the child env.
    Deny,
    /// A literal value (object `value:` or a resolved `$(…)`) — set directly,
    /// independent of the ambient env.
    Literal(String),
}

fn parse_env_array(
    items: &[Value],
    parent: Option<&EnvPolicy>,
    path: &str,
) -> Result<Vec<EnvEntry>, CompileError> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let p = child(path, &i.to_string());
        let s = as_str(item, &p)?;
        if s == "!..." {
            return Err(CompileError::shape(&p, SENTINEL_NEGATE_MSG));
        }
        if s == "..." {
            splice_env_inherit(parent, &mut out);
            continue;
        }
        let (pattern, deny) = match s.strip_prefix('!') {
            Some(rest) => (rest.to_string(), true),
            None => (s.to_string(), false),
        };
        // A `$(…)` in array form would have no key to bind to — array entries are
        // key/glob selectors, not values. Reject to avoid silent misuse.
        if resolve::has_substitution(&pattern) {
            return Err(CompileError::shape(
                &p,
                "`$(…)` is only valid as an object-form env value, not an array entry",
            ));
        }
        out.push(EnvEntry {
            pattern,
            action: if deny {
                EnvAction::Deny
            } else {
                EnvAction::Allow(None)
            },
            secret: !deny, // an allow defaults sensitive; a deny mark is irrelevant
            // The array form is a concise ALLOWLIST (pass-through-if-present),
            // never a required-var declaration — an exact key here means "permit
            // it", not "demand it" (required/optional is an object-form concept
            // via the `?` suffix). So array entries are always optional; without
            // this the canonical `["FOO", "BAR", "!*_TOKEN"]` would hard-error
            // whenever FOO is unset. Object plain-keys stay required.
            optional: true,
            format: None,
            key_match: KeyMatch::User,
            builtin: false,
        });
    }
    Ok(out)
}

fn parse_env_object(
    map: &serde_json::Map<String, Value>,
    ctx: &CompileCtx,
    parent: Option<&EnvPolicy>,
    path: &str,
) -> Result<Vec<EnvEntry>, CompileError> {
    let mut out = Vec::new();
    for (raw_key, val) in map {
        let p = child(path, raw_key);
        if raw_key == "!..." {
            return Err(CompileError::shape(&p, SENTINEL_NEGATE_MSG));
        }
        // `"..."` as an env-object key inherits the enclosing scope's env keys at
        // this position (positional last-match). `true` = inherit; a string is a
        // file-extends (frontend-resolved — deferred here, as elsewhere).
        if raw_key == "..." {
            match val {
                Value::Bool(true) => {
                    splice_env_inherit(parent, &mut out);
                    continue;
                }
                Value::String(reference) => {
                    return Err(CompileError::FileRefUnresolved {
                        path: p,
                        reference: reference.clone(),
                    });
                }
                _ => {
                    return Err(CompileError::shape(
                        &p,
                        "`\"...\"` value must be true (inherit the enclosing scope) or a file-ref",
                    ));
                }
            }
        }
        // A trailing `?` on the key marks it optional.
        let (key, optional) = match raw_key.strip_suffix('?') {
            Some(k) => (k.to_string(), true),
            None => (raw_key.clone(), false),
        };
        let entry = parse_env_object_value(key, optional, val, ctx, &p)?;
        out.push(entry);
    }
    Ok(out)
}

fn parse_env_object_value(
    key: String,
    optional: bool,
    val: &Value,
    ctx: &CompileCtx,
    path: &str,
) -> Result<EnvEntry, CompileError> {
    match val {
        Value::Bool(true) => Ok(EnvEntry {
            pattern: key,
            action: EnvAction::Allow(None),
            secret: true,
            optional,
            format: None,
            key_match: KeyMatch::User,
            builtin: false,
        }),
        Value::Bool(false) => Ok(EnvEntry {
            pattern: key,
            action: EnvAction::Deny,
            secret: true,
            optional,
            format: None,
            key_match: KeyMatch::User,
            builtin: false,
        }),
        Value::String(s) => parse_env_string_value(key, optional, s, ctx, path),
        Value::Object(extras) => parse_env_extras(key, optional, extras, ctx, path),
        _ => Err(CompileError::shape(
            path,
            "env value must be a boolean, a type string, \"$(…)\", or an object",
        )),
    }
}

fn parse_env_string_value(
    key: String,
    optional: bool,
    s: &str,
    ctx: &CompileCtx,
    path: &str,
) -> Result<EnvEntry, CompileError> {
    // `$(…)` resolver — trusted homes only.
    if resolve::has_substitution(s) {
        // Reject a glob-key literal BEFORE running the command (a glob key has no
        // single value to bind; without this the exec fires, then construct_env
        // rejects it — a wasted, surprising side effect).
        if is_glob(&key) {
            return Err(CompileError::shape(
                path,
                "`$(…)` cannot be bound to a glob key",
            ));
        }
        if !ctx.trusted {
            return Err(CompileError::untrusted_substitution(path));
        }
        let resolved = resolve::resolve_with(s, ctx.runner.as_ref())
            .map_err(|e| CompileError::substitution(path, &e))?;
        return Ok(EnvEntry {
            pattern: key,
            action: EnvAction::Literal(resolved),
            secret: true,
            optional,
            format: None,
            key_match: KeyMatch::User,
            builtin: false,
        });
    }
    // Otherwise a type from the grammar.
    let ty = parse_env_type(s).map_err(|e| CompileError::shape(path, &e))?;
    let format = ty.format();
    Ok(EnvEntry {
        pattern: key,
        action: EnvAction::Allow(Some(ty)),
        secret: true,
        optional,
        format,
        key_match: KeyMatch::User,
        builtin: false,
    })
}

/// The object extras form: `{ secret, public, format, value, optional }`.
fn parse_env_extras(
    key: String,
    optional_from_key: bool,
    extras: &serde_json::Map<String, Value>,
    ctx: &CompileCtx,
    path: &str,
) -> Result<EnvEntry, CompileError> {
    const ALLOWED: &[&str] = &["secret", "public", "format", "value", "optional"];
    for k in extras.keys() {
        if !ALLOWED.contains(&k.as_str()) {
            return Err(CompileError::shape(
                &child(path, k),
                &format!("unknown env option `{k}` (allowed: {})", ALLOWED.join(", ")),
            ));
        }
    }
    let public = extras
        .get("public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let secret = extras
        .get("secret")
        .and_then(Value::as_bool)
        .unwrap_or(!public);
    let optional = optional_from_key
        || extras
            .get("optional")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let ty = match extras.get("format") {
        Some(Value::String(f)) => {
            Some(parse_env_type(f).map_err(|e| CompileError::shape(&child(path, "format"), &e))?)
        }
        Some(_) => {
            return Err(CompileError::shape(
                &child(path, "format"),
                "format must be a string",
            ));
        }
        None => None,
    };
    let format = ty.as_ref().and_then(EnvType::format);
    // An explicit `value:` (optionally `$(…)`) overrides the ambient source.
    if let Some(v) = extras.get("value") {
        // A literal value has no single key to bind to under a glob — reject
        // before any `$(…)` runs.
        if is_glob(&key) {
            return Err(CompileError::shape(
                &child(path, "value"),
                "a literal `value` cannot be bound to a glob key",
            ));
        }
        let raw = as_str(v, &child(path, "value"))?;
        let resolved = if resolve::has_substitution(raw) {
            if !ctx.trusted {
                return Err(CompileError::untrusted_substitution(&child(path, "value")));
            }
            resolve::resolve_with(raw, ctx.runner.as_ref())
                .map_err(|e| CompileError::substitution(&child(path, "value"), &e))?
        } else {
            raw.to_string()
        };
        if let Some(t) = &ty {
            t.validate(&resolved)
                .map_err(|e| CompileError::validation(&child(path, "value"), &e))?;
        }
        return Ok(EnvEntry {
            pattern: key,
            action: EnvAction::Literal(resolved),
            secret,
            optional,
            format,
            key_match: KeyMatch::User,
            builtin: false,
        });
    }
    Ok(EnvEntry {
        pattern: key,
        action: EnvAction::Allow(ty),
        secret,
        optional,
        format,
        key_match: KeyMatch::User,
        builtin: false,
    })
}

/// The env `"..."` payload: inherit the enclosing scope's env at this position.
/// At an INNER scope (`parent = Some`) splice one `InheritedKeys` allow so the
/// child inherits exactly the resolved parent's keys (already secret-filtered by
/// the parent). At the OUTERMOST scope (`parent = None`) splice the built-in
/// curated baseline — the degenerate outermost case, ≡ `sandbox: true`'s env.
fn splice_env_inherit(parent: Option<&EnvPolicy>, out: &mut Vec<EnvEntry>) {
    match parent {
        Some(_) => out.push(EnvEntry {
            pattern: "...".to_string(),
            action: EnvAction::Allow(None),
            secret: false,
            optional: true,
            format: None,
            key_match: KeyMatch::InheritedKeys,
            builtin: true,
        }),
        None => splice_env_defaults(out),
    }
}

/// The built-in env base (outermost `"..."`): the secret DENIES followed by the
/// curated-baseline ALLOW. Ordered so the baseline allow is LAST — its verdict is
/// authoritative for baseline keys (so a bare `["..."]` ≡ the curated baseline,
/// i.e. `sandbox: true`'s env), while the secret denies bind only when a LATER
/// user entry re-broadens (e.g. `["*", "..."]`, which allows all then re-strips
/// secrets). All are `builtin` → excluded from the emitted user schema.
fn splice_env_defaults(out: &mut Vec<EnvEntry>) {
    let secret_deny = |pattern: String, key_match: KeyMatch| EnvEntry {
        pattern,
        action: EnvAction::Deny,
        secret: true,
        optional: false,
        format: None,
        key_match,
        builtin: true,
    };
    for tok in defaults::SECRET_SUBSTR_TOKENS {
        out.push(secret_deny(tok.to_string(), KeyMatch::SecretSubstr));
    }
    for tok in defaults::SECRET_SEGMENT_TOKENS {
        out.push(secret_deny(tok.to_string(), KeyMatch::SecretSegment));
    }
    for key in defaults::SECRET_ENV_KEYS {
        let pat = if key.ends_with('_') {
            format!("{key}*")
        } else {
            key.to_string()
        };
        out.push(secret_deny(pat, KeyMatch::SecretGlob));
    }
    // The curated allowlist as ONE allow entry (matches iff `baseline_allows`),
    // placed LAST so it is the authoritative verdict for the keys it admits.
    out.push(EnvEntry {
        pattern: "...".to_string(),
        action: EnvAction::Allow(None),
        secret: false,
        optional: true,
        format: None,
        key_match: KeyMatch::CuratedBaseline,
        builtin: true,
    });
}

/// Build the child env map + schema + withheld list from ordered entries.
/// Source keys are filtered last-match-wins; explicit-value entries are set
/// directly. A required exact key with no source value and no literal errors.
///
/// `parent` (an inner scope's resolved parent env) contributes two things: its
/// keys become candidate SOURCE keys (with the parent's resolved value winning
/// over ambient), and an `InheritedKeys` entry (spliced by `"..."`) admits
/// exactly those keys. At the outermost scope `parent` is `None` and the source
/// is the ambient env verbatim — behavior-identical to the single-term path.
fn construct_env(
    entries: &[EnvEntry],
    ctx: &CompileCtx,
    parent: Option<&EnvPolicy>,
    policy: &mut EnvPolicy,
) -> Result<(), CompileError> {
    // The value source: ambient, overlaid with the resolved parent's keys (parent
    // value wins — it is the already-resolved truth for an inherited key). Owned
    // only when a parent actually contributes keys, else the ambient env verbatim.
    let source_owned;
    let source: &BTreeMap<String, String> = match parent.filter(|p| !p.constructed.is_empty()) {
        Some(p) => {
            let mut m = ctx.ambient_env.clone();
            for (k, v) in &p.constructed {
                m.insert(k.clone(), v.clone());
            }
            source_owned = m;
            &source_owned
        }
        None => &ctx.ambient_env,
    };
    let parent_keys: BTreeSet<String> = parent
        .map(|p| p.constructed.keys().cloned().collect())
        .unwrap_or_default();

    // Compile a matcher per entry, honoring its `key_match`: user patterns are
    // case-sensitive globs, the built-in defaults case-insensitive / predicate.
    let matchers: Vec<KeyMatcher> = entries
        .iter()
        .map(|e| compile_key_matcher(e, &parent_keys))
        .collect();

    // 1. Literal-value entries: set directly + validate + schema. (Exact keys
    //    only; a glob key has no single value to bind.)
    for e in entries {
        if let EnvAction::Literal(v) = &e.action {
            if is_glob(&e.pattern) {
                return Err(CompileError::shape(
                    &e.pattern,
                    "a literal env value cannot be bound to a glob key",
                ));
            }
            policy.constructed.insert(e.pattern.clone(), v.clone());
        }
    }

    // 2. Source keys: last-match-wins over allow/deny entries.
    for (name, value) in source {
        if policy.constructed.contains_key(name) {
            continue; // a literal already claimed this key
        }
        let mut verdict: Option<&EnvEntry> = None;
        for (e, m) in entries.iter().zip(&matchers) {
            if m.hit(name) {
                verdict = Some(e);
            }
        }
        match verdict.map(|e| &e.action) {
            Some(EnvAction::Allow(ty)) => {
                if let Some(t) = ty {
                    t.validate(value)
                        .map_err(|err| CompileError::validation(name, &err))?;
                }
                policy.constructed.insert(name.clone(), value.clone());
            }
            _ => {
                // Deny, no match, or a literal (handled above) → withhold.
            }
        }
    }

    // 3. Required-key check: an exact-key Allow entry that is not optional, has no
    //    literal, and matched no source value → missing required var.
    for e in entries {
        if e.optional || is_glob(&e.pattern) {
            continue;
        }
        if matches!(e.action, EnvAction::Allow(_)) && !policy.constructed.contains_key(&e.pattern) {
            return Err(CompileError::missing_required(&e.pattern));
        }
    }

    // 4. Schema (one rule per non-deny, non-builtin entry) + withheld (source
    //    minus kept). Builtin baseline/inherited/secret entries carry no user
    //    validation or redaction mark, so they never enter the schema.
    let mut seen = BTreeSet::new();
    for e in entries {
        if e.builtin || matches!(e.action, EnvAction::Deny) {
            continue;
        }
        if seen.insert(e.pattern.clone()) {
            policy.schema.push(EnvRule {
                key: e.pattern.clone(),
                secret: e.secret,
                format: e.format,
                optional: e.optional,
            });
        }
    }
    policy.withheld = source
        .keys()
        .filter(|k| !policy.constructed.contains_key(*k))
        .cloned()
        .collect();
    Ok(())
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

/// A compiled env-key matcher — the runtime form of an entry's [`KeyMatch`].
enum KeyMatcher {
    /// A compiled glob (user case-sensitive, or a secret-KEY case-insensitive).
    Glob(GlobMatcher),
    /// Exact fallback when a user pattern fails to compile as a glob.
    Exact(String),
    /// A secret token matched as a case-insensitive substring.
    SecretSubstr(String),
    /// A secret token matched as a case-insensitive whole segment.
    SecretSegment(String),
    /// The curated-baseline predicate (`defaults::baseline_allows`).
    Baseline,
    /// Cross-scope inheritance: the key is in the resolved parent's env.
    InheritedKeys(BTreeSet<String>),
}

impl KeyMatcher {
    fn hit(&self, name: &str) -> bool {
        match self {
            KeyMatcher::Glob(m) => m.is_match(name),
            KeyMatcher::Exact(s) => s == name,
            KeyMatcher::SecretSubstr(word) => defaults::word_in_substr(word, name),
            KeyMatcher::SecretSegment(word) => defaults::word_is_segment(word, name),
            KeyMatcher::Baseline => defaults::baseline_allows(name),
            KeyMatcher::InheritedKeys(keys) => keys.contains(name),
        }
    }
}

/// Compile an entry's pattern into a [`KeyMatcher`] per its [`KeyMatch`] kind.
/// `parent_keys` is the resolved parent's env key set (empty at the outermost
/// scope), the match set for an `InheritedKeys` entry.
fn compile_key_matcher(e: &EnvEntry, parent_keys: &BTreeSet<String>) -> KeyMatcher {
    match e.key_match {
        KeyMatch::SecretSubstr => KeyMatcher::SecretSubstr(e.pattern.clone()),
        KeyMatch::SecretSegment => KeyMatcher::SecretSegment(e.pattern.clone()),
        KeyMatch::CuratedBaseline => KeyMatcher::Baseline,
        KeyMatch::InheritedKeys => KeyMatcher::InheritedKeys(parent_keys.clone()),
        KeyMatch::User | KeyMatch::SecretGlob => {
            let case_insensitive = matches!(e.key_match, KeyMatch::SecretGlob);
            GlobBuilder::new(&e.pattern)
                .case_insensitive(case_insensitive)
                .build()
                .map(|g| KeyMatcher::Glob(g.compile_matcher()))
                .unwrap_or_else(|_| KeyMatcher::Exact(e.pattern.clone()))
        }
    }
}

// ── shared helpers ────────────────────────────────────────────────────────────

/// Ensure the map has no keys beyond `allowed`; used by callers folding an
/// axis-bearing object. Exposed for the pipeline's granular-object validation.
pub fn reject_unknown_keys(
    map: &serde_json::Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> Result<(), CompileError> {
    for k in map.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(CompileError::shape(
                &child(path, k),
                &format!("unknown key `{k}` (allowed: {})", allowed.join(", ")),
            ));
        }
    }
    Ok(())
}

fn as_str<'a>(v: &'a Value, path: &str) -> Result<&'a str, CompileError> {
    v.as_str()
        .ok_or_else(|| CompileError::shape(path, "expected a string"))
}

fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}
