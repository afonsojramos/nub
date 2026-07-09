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
use globset::Glob;
use serde_json::Value;
use std::collections::BTreeSet;

// ── fs ───────────────────────────────────────────────────────────────────────

/// Fold the `fs` axis value into an [`FsPolicy`]. Array entries and object keys
/// are subtree-expanded (a bare path grants the node + `/**`); a glob-bearing
/// pattern is emitted verbatim. Access: array grants are ReadWrite (the concise
/// "these paths are fully usable" form); object values pick `"r"`/`"rw"`. A
/// `"..."` splices the generous-read + secret-deny defaults at its position.
pub fn fold_fs(value: &Value, ctx: &CompileCtx, path: &str) -> Result<FsPolicy, CompileError> {
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
                let s = as_str(item, &child(path, &i.to_string()))?;
                fold_fs_array_entry(s, ctx, &mut set.entries);
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

fn fold_fs_array_entry(s: &str, ctx: &CompileCtx, out: &mut Vec<FsRule>) {
    if s == "..." {
        splice_fs_defaults(ctx, out);
        return;
    }
    let (pattern, effect) = match s.strip_prefix('!') {
        Some(rest) => (rest, Effect::Deny),
        None => (s, Effect::Allow),
    };
    // Array grants are ReadWrite; denies deny both.
    push_fs_rules(pattern, effect, FsAccess::ReadWrite, ctx, out);
}

fn fold_fs_object_entry(
    key: &str,
    val: &Value,
    ctx: &CompileCtx,
    path: &str,
    out: &mut Vec<FsRule>,
) -> Result<(), CompileError> {
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

/// Splice the generous-read base + secret-deny defaults (the fs `"..."` payload).
fn splice_fs_defaults(ctx: &CompileCtx, out: &mut Vec<FsRule>) {
    out.push(defaults::generous_read_allow());
    out.extend(defaults::secret_read_denies(&ctx.homes));
}

// ── net ──────────────────────────────────────────────────────────────────────

/// Fold the `net` axis into a [`NetPolicy`]. Entries are host globs or CIDRs;
/// `!` denies; `"..."` splices the trusted-host default allows. `net: true`
/// disables enforcement; `net: false` denies all egress.
pub fn fold_net(value: &Value, path: &str) -> Result<NetPolicy, CompileError> {
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
                let s = as_str(item, &child(path, &i.to_string()))?;
                fold_net_entry(s, &mut policy.rules)?;
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                let effect = net_value_effect(val, &child(path, key))?;
                push_net_rule(key, effect, &mut policy.rules)?;
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

fn fold_net_entry(s: &str, out: &mut Vec<NetRule>) -> Result<(), CompileError> {
    if s == "..." {
        // No default trusted-host allowlist is committed in Stage 1 (the
        // build-jail baseline owns it). `"..."` in net currently splices nothing
        // — a self-contained net policy. Documented so a later reader wires the
        // baseline here rather than assuming it is silently applied.
        return Ok(());
    }
    let (pattern, effect) = match s.strip_prefix('!') {
        Some(rest) => (rest, Effect::Deny),
        None => (s, Effect::Allow),
    };
    push_net_rule(pattern, effect, out)
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
fn push_net_rule(target: &str, effect: Effect, out: &mut Vec<NetRule>) -> Result<(), CompileError> {
    let net_target = if target.contains('/') {
        match target.parse::<ipnet::IpNet>() {
            Ok(net) => NetTarget::Cidr(net),
            Err(e) => {
                return Err(CompileError::shape(
                    "net",
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
pub fn fold_env(value: &Value, ctx: &CompileCtx, path: &str) -> Result<EnvPolicy, CompileError> {
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
            let entries = parse_env_array(items, path)?;
            construct_env(&entries, ctx, &mut policy)?;
        }
        Value::Object(map) => {
            let entries = parse_env_object(map, ctx, path)?;
            construct_env(&entries, ctx, &mut policy)?;
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

fn parse_env_array(items: &[Value], path: &str) -> Result<Vec<EnvEntry>, CompileError> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let s = as_str(item, &child(path, &i.to_string()))?;
        if s == "..." {
            splice_env_defaults(&mut out);
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
                &child(path, &i.to_string()),
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
            optional: false,
            format: None,
        });
    }
    Ok(out)
}

fn parse_env_object(
    map: &serde_json::Map<String, Value>,
    ctx: &CompileCtx,
    path: &str,
) -> Result<Vec<EnvEntry>, CompileError> {
    let mut out = Vec::new();
    for (raw_key, val) in map {
        let p = child(path, raw_key);
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
        }),
        Value::Bool(false) => Ok(EnvEntry {
            pattern: key,
            action: EnvAction::Deny,
            secret: true,
            optional,
            format: None,
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
        });
    }
    Ok(EnvEntry {
        pattern: key,
        action: EnvAction::Allow(ty),
        secret,
        optional,
        format,
    })
}

/// The env `"..."` payload: the default secret denies (name-token + prefix
/// matches), spliced as trailing deny entries.
fn splice_env_defaults(out: &mut Vec<EnvEntry>) {
    for tok in defaults::SECRET_ENV_TOKENS {
        out.push(EnvEntry {
            pattern: format!("*{tok}*"),
            action: EnvAction::Deny,
            secret: true,
            optional: false,
            format: None,
        });
    }
    for key in defaults::SECRET_ENV_KEYS {
        let pat = if key.ends_with('_') {
            format!("{key}*")
        } else {
            key.to_string()
        };
        out.push(EnvEntry {
            pattern: pat,
            action: EnvAction::Deny,
            secret: true,
            optional: false,
            format: None,
        });
    }
}

/// Build the child env map + schema + withheld list from ordered entries.
/// Ambient keys are filtered last-match-wins; explicit-value entries are set
/// directly. A required exact key with no ambient value and no literal errors.
fn construct_env(
    entries: &[EnvEntry],
    ctx: &CompileCtx,
    policy: &mut EnvPolicy,
) -> Result<(), CompileError> {
    // Compile a matcher per entry pattern (case-SENSITIVE — env keys are).
    let matchers: Vec<Option<globset::GlobMatcher>> = entries
        .iter()
        .map(|e| Glob::new(&e.pattern).ok().map(|g| g.compile_matcher()))
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

    // 2. Ambient keys: last-match-wins over allow/deny entries.
    for (name, value) in &ctx.ambient_env {
        if policy.constructed.contains_key(name) {
            continue; // a literal already claimed this key
        }
        let mut verdict: Option<&EnvEntry> = None;
        for (e, m) in entries.iter().zip(&matchers) {
            let hit = match m {
                Some(mm) => mm.is_match(name),
                None => &e.pattern == name,
            };
            if hit {
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
    //    literal, and matched no ambient value → missing required var.
    for e in entries {
        if e.optional || is_glob(&e.pattern) {
            continue;
        }
        if matches!(e.action, EnvAction::Allow(_)) && !policy.constructed.contains_key(&e.pattern) {
            return Err(CompileError::missing_required(&e.pattern));
        }
    }

    // 4. Schema (one rule per non-deny entry) + withheld (ambient minus kept).
    let mut seen = BTreeSet::new();
    for e in entries {
        if matches!(e.action, EnvAction::Deny) {
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
    policy.withheld = ctx
        .ambient_env
        .keys()
        .filter(|k| !policy.constructed.contains_key(*k))
        .cloned()
        .collect();
    Ok(())
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
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
