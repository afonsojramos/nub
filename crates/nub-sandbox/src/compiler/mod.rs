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

mod defaults;
mod env_grammar;
mod fold;
pub mod layering;
mod preset;
mod resolve;
pub mod scope;

pub use resolve::{CommandRunner, ShellRunner};

use crate::matcher::path::Homes;
use crate::policy::{Effect, FsPolicy, NetPolicy, SandboxPolicy};
use serde_json::Value;
use std::collections::BTreeMap;

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

/// Compile a `sandbox` surface block into a resolved [`SandboxPolicy`].
pub fn compile(surface: &Value, ctx: &CompileCtx) -> Result<SandboxPolicy, CompileError> {
    match surface {
        // `false` — fully unjail: every axis relaxed.
        Value::Bool(false) => Ok(unjailed(ctx)),
        // `true` — secure defaults per axis (see `secure_default`).
        Value::Bool(true) => secure_default(ctx),
        // `"<preset>"` (bare) or `"./file"` (path-like).
        Value::String(s) => match classify_string(s) {
            StringKind::Preset => {
                let expanded = preset::resolve(s)?;
                compile_object(&expanded, ctx)
            }
            StringKind::FileRef => Err(CompileError::FileRefUnresolved {
                path: String::new(),
                reference: s.clone(),
            }),
        },
        Value::Object(_) => compile_object(surface, ctx),
        _ => Err(CompileError::shape(
            "",
            "sandbox must be a boolean, a preset name, a file-ref, or a { fs, net, env } object",
        )),
    }
}

/// Fold a granular `{ fs, net, env }` object; an absent axis takes its secure
/// default (so a partial object still confines the axes it omits).
fn compile_object(surface: &Value, ctx: &CompileCtx) -> Result<SandboxPolicy, CompileError> {
    let obj = surface
        .as_object()
        .ok_or_else(|| CompileError::shape("", "expected a { fs, net, env } object"))?;
    fold::reject_unknown_keys(obj, &["fs", "net", "env"], "")?;

    // An absent axis in a granular object is UNCONFINED (relaxed) — the granular
    // form confines what you name; use `sandbox: true` for blanket secure
    // defaults. This is the "boolean is the de-nesting mechanism" contract.
    let fs = match obj.get("fs") {
        Some(v) => fold::fold_fs(v, ctx, "fs")?,
        None => relaxed_fs(),
    };
    let net = match obj.get("net") {
        Some(v) => fold::fold_net(v, "net")?,
        None => relaxed_net(),
    };
    let env = match obj.get("env") {
        Some(v) => fold::fold_env(v, ctx, "env")?,
        None => crate::policy::EnvPolicy::default(), // enforce=false → inherit
    };
    Ok(SandboxPolicy {
        fs,
        net,
        env,
        pid: Default::default(),
    })
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
    // write grant.
    fold::fold_fs(&Value::Array(vec![Value::String("...".into())]), ctx, "fs")
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
