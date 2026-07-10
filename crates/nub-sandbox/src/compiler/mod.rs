//! The config compiler (Boundary A): the ONLY code that understands surface
//! syntax. Input is already-parsed data (a `serde_json::Value` for the `sandbox`
//! block + a [`CompileCtx`] of host-provided paths/env); output is a fully
//! resolved [`SandboxPolicy`]. The compiler NEVER reads config files — it may
//! canonicalize fs paths (a filesystem read, not a config read).
//!
//! Pipeline (design.md §2.2): wrapper trichotomy → preset expansion → per-axis
//! fold (with `"..."` spread + last-match-wins order) → env grammar + `$(…)` →
//! emit. Scope resolution and tighten-only layering live in sibling modules for
//! the future project frontend; the `--sandbox` entry is single-block.

mod clobber;
mod defaults;
mod env_grammar;
mod fold;
pub mod layering;
mod preset;
mod resolve;
pub mod scope;

pub use resolve::{CommandRunner, ShellRunner};

use crate::matcher::path::Homes;
use crate::policy::{Effect, EnvPolicy, FsPolicy, NetPolicy, SandboxPolicy};
use serde_json::Value;
use std::collections::BTreeMap;

/// A non-fatal compile diagnostic. Distinct from [`CompileError`]: the policy
/// still compiles, but something in the surface is a smell worth surfacing — today
/// only the clobber warning (a later array entry that fully shadows an earlier
/// one, making it dead). Carried on the side of the result, NEVER in the resolved
/// [`SandboxPolicy`] IR (backends consume policy, not diagnostics).
#[derive(Debug, Clone, PartialEq)]
pub struct CompileWarning {
    /// The surface path the warning occurred at (e.g. `fs`, `env`).
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for CompileWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sandbox.{}: {}", self.path, self.message)
    }
}

/// Host-provided context for a compile. All fields are ALREADY-PARSED data — the
/// engine stays PM-pure (Boundary B): nub-cli does file discovery/parse and the
/// ambient-env snapshot, then hands them here.
pub struct CompileCtx {
    /// Per-OS home anchors symbolic roots expand against.
    pub homes: Homes,
    /// The current working directory (for diagnostics / relative anchoring).
    pub cwd: std::path::PathBuf,
    /// Whether `$(…)` command substitution is permitted. TRUE only for the user's
    /// own trusted config (`nub.jsonc` / `scriptsMeta`); FALSE for a
    /// `dependenciesMeta` grant — an untrusted `$(…)` is a hard error, never exec.
    pub trusted: bool,
    /// The ambient env snapshot the child env is constructed from.
    pub ambient_env: BTreeMap<String, String>,
    /// The `$(…)` command runner (production shells out; tests inject a stub).
    pub runner: Box<dyn CommandRunner>,
}

impl CompileCtx {
    /// A ctx with the real shell runner and the given homes/env/trust.
    pub fn new(
        homes: Homes,
        cwd: std::path::PathBuf,
        trusted: bool,
        ambient_env: BTreeMap<String, String>,
    ) -> Self {
        Self {
            homes,
            cwd,
            trusted,
            ambient_env,
            runner: Box::new(ShellRunner),
        }
    }
}

/// A compile failure. Every variant carries the surface path it occurred at so
/// diagnostics point at the offending field.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// A structural/shape violation (wrong type, unknown key, bad ladder value).
    Shape { path: String, message: String },
    /// A `"<preset>"` name not in the closed table.
    UnknownPreset {
        name: String,
        supported: Vec<String>,
    },
    /// A nested file-ref (`"./x.json"`) inside a compiled block — the engine does
    /// not resolve file-refs (the caller loads the file; a nested ref is deferred).
    FileRefUnresolved { path: String, reference: String },
    /// A `$(…)` in an untrusted home (`dependenciesMeta`).
    UntrustedSubstitution { path: String },
    /// A `$(…)` that failed to run.
    Substitution { path: String, message: String },
    /// A value failed its env type validation.
    Validation { path: String, message: String },
    /// A required (non-optional, no-default) env key had no value.
    MissingRequired { key: String },
}

impl CompileError {
    pub(crate) fn shape(path: &str, message: &str) -> Self {
        Self::Shape {
            path: path.to_string(),
            message: message.to_string(),
        }
    }
    pub(crate) fn unknown_preset(name: &str, supported: &[&str]) -> Self {
        Self::UnknownPreset {
            name: name.to_string(),
            supported: supported.iter().map(|s| s.to_string()).collect(),
        }
    }
    pub(crate) fn untrusted_substitution(path: &str) -> Self {
        Self::UntrustedSubstitution {
            path: path.to_string(),
        }
    }
    pub(crate) fn substitution(path: &str, message: &str) -> Self {
        Self::Substitution {
            path: path.to_string(),
            message: message.to_string(),
        }
    }
    pub(crate) fn validation(path: &str, message: &str) -> Self {
        Self::Validation {
            path: path.to_string(),
            message: message.to_string(),
        }
    }
    pub(crate) fn missing_required(key: &str) -> Self {
        Self::MissingRequired {
            key: key.to_string(),
        }
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape { path, message } => write!(f, "sandbox.{path}: {message}"),
            Self::UnknownPreset { name, supported } => write!(
                f,
                "unknown sandbox preset `{name}` — supported: {}",
                supported.join(", ")
            ),
            Self::FileRefUnresolved { path, reference } => write!(
                f,
                "sandbox.{path}: nested file-ref `{reference}` is not resolved by the engine"
            ),
            Self::UntrustedSubstitution { path } => write!(
                f,
                "sandbox.{path}: `$(…)` command substitution is not permitted in an untrusted (dependenciesMeta) grant"
            ),
            Self::Substitution { path, message } => write!(f, "sandbox.{path}: {message}"),
            Self::Validation { path, message } => write!(f, "sandbox.{path}: {message}"),
            Self::MissingRequired { key } => write!(f, "required env var `{key}` is not set"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile a `sandbox` surface block into a resolved [`SandboxPolicy`]. Discards
/// any [`CompileWarning`]s; use [`compile_with_warnings`] to surface them.
pub fn compile(surface: &Value, ctx: &CompileCtx) -> Result<SandboxPolicy, CompileError> {
    compile_with_warnings(surface, ctx).map(|(policy, _)| policy)
}

/// Compile a `sandbox` surface block, returning the resolved policy AND any
/// non-fatal warnings (the clobber smell). Single-term entry: the `"..."` payload
/// resolves against the built-in base (there is no parent scope).
pub fn compile_with_warnings(
    surface: &Value,
    ctx: &CompileCtx,
) -> Result<(SandboxPolicy, Vec<CompileWarning>), CompileError> {
    let mut warnings = Vec::new();
    let policy = compile_scope(surface, None, ctx, &mut warnings)?;
    Ok((policy, warnings))
}

/// Resolve ONE scope's surface against its resolved `parent` (the enclosing
/// scope's policy; `None` at the outermost scope → the built-in base). The
/// wrapper trichotomy; a granular object goes to [`compile_object`]. This is the
/// per-scope primitive the chain resolver ([`scope::resolve_chain`]) drives.
pub(crate) fn compile_scope(
    surface: &Value,
    parent: Option<&SandboxPolicy>,
    ctx: &CompileCtx,
    warnings: &mut Vec<CompileWarning>,
) -> Result<SandboxPolicy, CompileError> {
    match surface {
        // `false` — fully unjail: every axis relaxed.
        Value::Bool(false) => Ok(unjailed(ctx)),
        // `true` — secure defaults per axis (see `secure_default`).
        Value::Bool(true) => secure_default(ctx),
        // `"<preset>"` (bare) or `"./file"` (path-like).
        Value::String(s) => match classify_string(s) {
            StringKind::Preset => {
                let expanded = preset::resolve(s)?;
                compile_object(&expanded, parent, ctx, warnings)
            }
            StringKind::FileRef => Err(CompileError::FileRefUnresolved {
                path: String::new(),
                reference: s.clone(),
            }),
        },
        Value::Object(_) => compile_object(surface, parent, ctx, warnings),
        _ => Err(CompileError::shape(
            "",
            "sandbox must be a boolean, a preset name, a file-ref, or a { fs, net, env } object",
        )),
    }
}

/// Fold a granular `{ fs, net, env }` object. A present block is a COMPLETE
/// statement: an axis it does NOT list FLOORS (deny fs, deny-all-enforcing net,
/// strip env) — least-exposure, fails closed. An object-level `"..."` key
/// (`{ "...": true }`) opts every UNLISTED axis into inheriting the enclosing
/// scope's base instead of flooring; a LISTED axis's own `"..."` inherits that
/// axis. So `{}` = deny-all; `{ "fs": [...] }` floors net+env; `{ "...": true }`
/// ≡ the enclosing base for all axes.
fn compile_object(
    surface: &Value,
    parent: Option<&SandboxPolicy>,
    ctx: &CompileCtx,
    warnings: &mut Vec<CompileWarning>,
) -> Result<SandboxPolicy, CompileError> {
    let obj = surface
        .as_object()
        .ok_or_else(|| CompileError::shape("", "expected a { fs, net, env } object"))?;
    fold::reject_unknown_keys(obj, &["fs", "net", "env", "..."], "")?;
    let inherit_base = object_spread(obj.get("..."))?;

    // Clobber detection runs per ARRAY axis (a total shadow between two entries of
    // the SAME array — D2b/D6); object forms have unique keys, so their granular
    // overrides are the intended idiom.
    if let Some(Value::Array(items)) = obj.get("fs") {
        clobber::detect_fs(items, &ctx.homes, "fs", warnings);
    }
    if let Some(Value::Array(items)) = obj.get("net") {
        clobber::detect_net(items, "net", warnings);
    }
    if let Some(Value::Array(items)) = obj.get("env") {
        clobber::detect_env(items, "env", warnings);
    }

    let fs = match obj.get("fs") {
        Some(v) => fold::fold_fs(v, ctx, "fs", parent.map(|p| &p.fs))?,
        None if inherit_base => inherit_fs(parent, ctx),
        None => floor_fs(),
    };
    let net = match obj.get("net") {
        Some(v) => fold::fold_net(v, "net", parent.map(|p| &p.net))?,
        None if inherit_base => inherit_net(parent),
        None => floor_net(),
    };
    let env = match obj.get("env") {
        Some(v) => fold::fold_env(v, ctx, "env", parent.map(|p| &p.env))?,
        None if inherit_base => inherit_env(parent, ctx),
        None => floor_env(ctx),
    };
    Ok(SandboxPolicy {
        fs,
        net,
        env,
        pid: Default::default(),
    })
}

/// Parse a top-level object `"..."` key. `true` = inherit the enclosing base for
/// unlisted axes; a string = a file-extends (frontend-resolved — deferred here);
/// anything else is a shape error. Absent = complete statement (floor unlisted).
fn object_spread(v: Option<&Value>) -> Result<bool, CompileError> {
    match v {
        None => Ok(false),
        Some(Value::Bool(true)) => Ok(true),
        Some(Value::String(reference)) => Err(CompileError::FileRefUnresolved {
            path: "...".to_string(),
            reference: reference.clone(),
        }),
        Some(_) => Err(CompileError::shape(
            "...",
            "`\"...\"` value must be true (inherit the enclosing scope) or a file-ref",
        )),
    }
}

/// An unlisted axis under an object-level `"..."`: inherit the resolved parent's
/// axis at an inner scope, or the built-in base (≡ `sandbox: true`'s axis) at the
/// outermost scope.
fn inherit_fs(parent: Option<&SandboxPolicy>, ctx: &CompileCtx) -> FsPolicy {
    parent
        .map(|p| p.fs.clone())
        .unwrap_or_else(|| secure_default_fs(ctx))
}
fn inherit_net(parent: Option<&SandboxPolicy>) -> NetPolicy {
    parent
        .map(|p| p.net.clone())
        .unwrap_or_else(secure_default_net)
}
fn inherit_env(parent: Option<&SandboxPolicy>, ctx: &CompileCtx) -> EnvPolicy {
    parent
        .map(|p| p.env.clone())
        .unwrap_or_else(|| secure_default_env(ctx))
}

/// The complete-statement FLOOR for an unlisted axis — the security inversion.
/// fs: deny-all (`FsRuleSet::default` is a deny base with no entries). net:
/// deny-all, ENFORCING. env: strip-all (enforce, empty constructed, everything
/// withheld) — identical to folding the axis with `false`.
fn floor_fs() -> FsPolicy {
    FsPolicy::default()
}
fn floor_net() -> NetPolicy {
    NetPolicy {
        enforce: true,
        rules: Vec::new(),
        default_effect: Effect::Deny,
    }
}
fn floor_env(ctx: &CompileCtx) -> EnvPolicy {
    EnvPolicy {
        enforce: true,
        constructed: BTreeMap::new(),
        schema: Vec::new(),
        withheld: ctx.ambient_env.keys().cloned().collect(),
    }
}

/// `sandbox: false` — every axis relaxed. The explicit escape hatch.
fn unjailed(_ctx: &CompileCtx) -> SandboxPolicy {
    SandboxPolicy {
        fs: relaxed_fs(),
        net: relaxed_net(),
        env: crate::policy::EnvPolicy::default(), // enforce=false → inherit
        pid: Default::default(),
    }
}

/// A relaxed fs axis: allow-all base, no entries.
fn relaxed_fs() -> FsPolicy {
    let mut fs = FsPolicy::default();
    fs.rules.default_effect = Effect::Allow;
    fs
}

/// A relaxed net axis: not enforcing.
fn relaxed_net() -> NetPolicy {
    NetPolicy {
        enforce: false,
        ..Default::default()
    }
}

/// `sandbox: true` — secure defaults per axis. PROVISIONAL posture (documented):
/// the exact runtime secure-default is the deferred runtime-frontend's product
/// call; the frontend-less engine only needs a safe, explicit baseline since the
/// conformance fixtures drive explicit policies. Today: generous read minus
/// secrets + no write, deny-all net, stripped env.
fn secure_default(ctx: &CompileCtx) -> Result<SandboxPolicy, CompileError> {
    Ok(SandboxPolicy {
        fs: secure_default_fs(ctx),
        net: secure_default_net(),
        env: secure_default_env(ctx),
        pid: Default::default(),
    })
}

/// `sandbox: true` env — the curated non-secret baseline (usable + secret-free).
fn secure_default_env(ctx: &CompileCtx) -> crate::policy::EnvPolicy {
    let constructed = defaults::curated_baseline_env(&ctx.ambient_env);
    let withheld = ctx
        .ambient_env
        .keys()
        .filter(|k| !constructed.contains_key(*k))
        .cloned()
        .collect();
    crate::policy::EnvPolicy {
        enforce: true,
        constructed,
        schema: Vec::new(),
        withheld,
    }
}

fn secure_default_fs(ctx: &CompileCtx) -> FsPolicy {
    // Equivalent to `fs: ["..."]` — the generous-read + secret-deny defaults, no
    // write grant. Outermost scope → `parent = None`.
    fold::fold_fs(
        &Value::Array(vec![Value::String("...".into())]),
        ctx,
        "fs",
        None,
    )
    .expect("`[\"...\"]` fs default always folds")
}

fn secure_default_net() -> NetPolicy {
    // Enforce with a deny-all base and no committed allowlist (the build-jail
    // baseline owns the trusted-host allows).
    NetPolicy {
        enforce: true,
        rules: Vec::new(),
        default_effect: Effect::Deny,
    }
}

enum StringKind {
    Preset,
    FileRef,
}

/// Disambiguate a `sandbox` string: a path-like string (leading `./`/`../`/`/`/`~`,
/// or carrying a file extension) is a file-ref; a bare identifier is a preset. Must
/// stay byte-identical to nub-cli's `project_config::classify_sandbox_string` — the
/// two classify the same surface string and a divergence would route it differently
/// through the skeleton vs the engine.
fn classify_string(s: &str) -> StringKind {
    let path_like = s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
        || s.starts_with('~')
        || std::path::Path::new(s).extension().is_some();
    if path_like {
        StringKind::FileRef
    } else {
        StringKind::Preset
    }
}
