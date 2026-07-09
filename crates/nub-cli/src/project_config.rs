//! The project-level `nub.jsonc` — nub's per-project settings file, discovered
//! up-tree from the run's working directory. Distinct from the GLOBAL file
//! (`~/.config/nub/nub.jsonc`, [`crate::config`]): the global file is nub's own
//! durable-settings home and is read best-effort (a malformed file degrades to
//! the default); the PROJECT file is authored by the user for one project and is
//! read FAIL-LOUD (an unknown key or malformed value is an error, per the bunfig
//! silent-no-op lesson — [`.fray/nub-config-spec.md`]).
//!
//! Dialect: JSONC (JSON + comments + trailing commas), the tsconfig dialect,
//! parsed through `jsonc_parser::parse_to_serde_value` — the same reader the
//! global file uses. camelCase keys. `$schema` is the one blessed non-field key
//! (accepted + ignored). Every other unrecognized key fails loud.
//!
//! ## The discovery gate (the ONE flip point)
//!
//! Project-file discovery is DISABLED by default. [`discovery_enabled`] is the
//! single flip point: it returns `false` today, so [`load_project_config`] — the
//! sole production entry — never discovers or reads a project file, and nub's
//! effective config is the global file only (today's behavior, byte-for-byte).
//! The eventual nub.jsonc release flips that one function body to `true`.
//!
//! To keep the gated path from being dead code behind a `const false`, the reader
//! is split in two layers: the pure [`discover_project_config`] /
//! [`read_project_config_at`] functions are UNGATED and carry the bulk of the
//! test coverage; only [`load_project_config`] consults the gate. Tests exercise
//! the production gate through the `#[cfg(test)]` [`with_project_config_enabled`]
//! seam.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jsonc_parser::ParseOptions;
use serde_json::Value;

use crate::config::ImplicitDlx;

/// The filename discovered up-tree. `nub.json` is NEVER accepted (no dual-name
/// ambiguity) — one name, matching the global file.
const FILE_NAME: &str = "nub.jsonc";

// ─────────────────────────────────────────────────────────────────────────────
// Error type — fail-loud, with a JSON path so a bad file self-describes.
// ─────────────────────────────────────────────────────────────────────────────

/// A project-config load failure. Carries a dotted JSON path (`install.hoist`)
/// so the message points at the offending key without the user re-deriving it.
#[derive(Debug)]
pub enum ConfigError {
    /// The file exists but could not be read (I/O).
    Io(std::io::Error),
    /// The file is not valid JSONC.
    Parse(String),
    /// A key nub does not recognize at `path` (fail-loud, not a silent no-op).
    UnknownKey { path: String, key: String },
    /// The value at `path` has the wrong JSON type.
    Type {
        path: String,
        expected: &'static str,
    },
    /// The value at `path` is the right type but semantically invalid.
    Value { path: String, message: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "reading {FILE_NAME}: {e}"),
            ConfigError::Parse(m) => write!(f, "parsing {FILE_NAME}: {m}"),
            ConfigError::UnknownKey { path, key } => {
                write!(f, "unknown key `{key}` in {path} of {FILE_NAME}")
            }
            ConfigError::Type { path, expected } => {
                write!(f, "`{path}` in {FILE_NAME} must be {expected}")
            }
            ConfigError::Value { path, message } => {
                write!(f, "`{path}` in {FILE_NAME}: {message}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

type Result<T> = std::result::Result<T, ConfigError>;

// ─────────────────────────────────────────────────────────────────────────────
// The typed config shape (Slice 1). Runtime top-levels are fully typed; the
// `sandbox`/`install`/`dlx` blocks are shape-validated skeletons — the sandbox
// engine's real compiler (Phase 1) owns their resolution, this layer only proves
// the schema is right so the future unflag turns on a validated surface.
// ─────────────────────────────────────────────────────────────────────────────

/// The parsed, validated `nub.jsonc`. Absent fields are `None`/empty; the
/// consumer applies built-in defaults. `$schema` is intentionally not stored
/// (accepted + ignored — editor metadata with no runtime effect).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProjectConfig {
    // ── runtime top-levels (bunfig-style flat) ──
    pub node_compat: Option<bool>,
    pub preload: Vec<String>,
    pub node_options: Vec<String>,
    pub v8_flags: Vec<String>,
    pub env: Option<EnvSetting>,
    pub define: BTreeMap<String, String>,
    pub loader: BTreeMap<String, String>,
    pub conditions: Vec<String>,
    pub tsconfig: Option<String>,
    pub verify_deps_before_run: Option<VerifyDepsBeforeRun>,

    // ── the default sandbox (skeleton — not consumed in this epic) ──
    pub sandbox: Option<SandboxSetting>,

    // ── install phase (skeleton for engine-coupled fields) ──
    pub install: InstallConfig,

    // ── nubx / dlx ──
    pub dlx: DlxConfig,
}

/// The `env` field's tri-state (merged `env` + `envFile`, per the spec):
/// `true` = today's default discovery, `false` = disable all env loading,
/// string / string[] = an exclusive source list (old `envFile` semantics).
/// Source strings are stored RAW; `${VAR}`/`$VAR` expansion is applied at the
/// wiring boundary (it references the process env, which the parser must not).
#[derive(Debug, Clone, PartialEq)]
pub enum EnvSetting {
    /// `true` — default `.env*` discovery.
    Default,
    /// `false` — disable all env-file loading.
    Disabled,
    /// A string / string[] exclusive source list.
    Sources(Vec<String>),
}

/// `verifyDepsBeforeRun` — pnpm's literal field name + value space (zero-
/// translation compat). `Enabled(false)` is pnpm's `false`; the string arms are
/// pnpm's `"install"|"warn"|"error"|"prompt"`.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyDepsBeforeRun {
    Enabled(bool),
    Install,
    Warn,
    Error,
    Prompt,
}

/// The `install` block. `node_linker` is the collapsed flat layout enum; the rest
/// are engine-coupled and parse-validated here (their effect wiring is a later,
/// aube-coupled effort).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct InstallConfig {
    pub node_linker: Option<NodeLinker>,
    pub symlink_disable_pattern: Vec<String>,
    pub hoist: Option<Hoist>,
    pub minimum_release_age: Option<Duration>,
    pub minimum_release_age_exclude: Vec<String>,
    pub node_options: Vec<String>,
    pub sandbox: Option<SandboxSetting>,
}

/// The flat layout enum (collapsed 2026-07-08). `symlink` (nub's default global
/// virtual store) / `isolated` (pnpm-parity project-local) / `hoisted` (flat real
/// dirs). `pnp` is reserved for PnP-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeLinker {
    Symlink,
    Isolated,
    Hoisted,
    Pnp,
}

/// `hoist` — pnpm-literal `boolean | string[]`. `Bool(false)` = strict,
/// `Bool(true)` ≡ pnpm `['*']`, `Patterns` = the pattern list.
#[derive(Debug, Clone, PartialEq)]
pub enum Hoist {
    Bool(bool),
    Patterns(Vec<String>),
}

/// The `dlx` block — nubx's own security posture. `consent` reuses the global
/// file's [`ImplicitDlx`] enum; `env` reuses the top-level tri-state.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DlxConfig {
    pub consent: Option<ImplicitDlx>,
    pub sandbox: Option<SandboxSetting>,
    pub env: Option<EnvSetting>,
}

// ── sandbox skeleton ─────────────────────────────────────────────────────────

/// The `sandbox` wrapper trichotomy (skeleton). Shape-validated here; the Phase-1
/// compiler owns resolution (presets → policy, spread, layering, env grammar).
/// Preset vs file-ref disambiguation: a path-like string (leading `./`/`../`/`/`/
/// `~`, or carrying an extension) is a file-ref; a bare identifier is a preset.
#[derive(Debug, Clone, PartialEq)]
pub enum SandboxSetting {
    /// `false` — fully unjail.
    Disabled,
    /// `true` — secure defaults / inherit.
    Enabled,
    /// A bare-identifier preset name (e.g. `"build-jail"`).
    Preset(String),
    /// A `"./file.json"` policy-file reference.
    FileRef(String),
    /// The granular `{ fs, net, env }` object form.
    Granular(SandboxAxes),
}

/// The three sandbox axes. Each is independently optional; an absent axis inherits
/// the axis default.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SandboxAxes {
    pub fs: Option<SandboxAxis>,
    pub net: Option<SandboxAxis>,
    pub env: Option<SandboxAxis>,
}

/// One axis value. Both surface forms are accepted (spec: every axis takes
/// `false | true | array | pattern-keyed object`). Entry semantics (polarity,
/// per-axis value ladder, env grammar) are the Phase-1 compiler's job — the
/// object form keeps its raw values so nothing is lost before then.
#[derive(Debug, Clone, PartialEq)]
pub enum SandboxAxis {
    Bool(bool),
    /// Array form: ordered glob entries (validated all-strings).
    Array(Vec<String>),
    /// Pattern-keyed object form: `{ "<pattern>": <value> }`, values kept raw.
    Object(BTreeMap<String, Value>),
}

// ─────────────────────────────────────────────────────────────────────────────
// The discovery gate.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
thread_local! {
    /// Per-thread override for the production gate. Set only through
    /// [`with_project_config_enabled`]; a thread-local keeps parallel tests
    /// hermetic (no process-global mutation, no cross-test leak).
    static FORCE_ENABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// THE ONE FLIP POINT. Project-file discovery is off until nub.jsonc ships; the
/// unflag PR changes this body to `true`. Not a user knob and not an env read —
/// a private compile-time gate consulted by exactly one caller
/// ([`load_project_config`]).
fn discovery_enabled() -> bool {
    #[cfg(test)]
    if FORCE_ENABLE.with(|c| c.get()) {
        return true;
    }
    false
}

/// Run `f` with the production discovery gate forced on. Test-only; mirrors the
/// hermetic-guard shape of [`crate::config`]'s `with_config_home`.
#[cfg(test)]
pub(crate) fn with_project_config_enabled<T>(f: impl FnOnce() -> T) -> T {
    FORCE_ENABLE.with(|c| c.set(true));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    FORCE_ENABLE.with(|c| c.set(false));
    match out {
        Ok(v) => v,
        Err(p) => std::panic::resume_unwind(p),
    }
}

/// Walk up from `start` (inclusive) to the filesystem root, returning the first
/// directory that holds a `nub.jsonc`. Ungated + pure over the path — the
/// discovery mechanism, exercised directly by tests.
pub fn discover_project_config(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parse + validate the `nub.jsonc` at `path`. Ungated + pure (a filesystem read,
/// never a discovery decision) — this is where the bulk of coverage lives.
/// FAIL-LOUD: an unknown key or malformed value is a [`ConfigError`], NOT a
/// silent degrade (unlike the best-effort global reader).
pub fn read_project_config_at(path: &Path) -> Result<ProjectConfig> {
    let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    parse_project_config(&text)
}

/// Parse + validate from raw JSONC text. Split out so tests can hit the validator
/// without touching the filesystem.
pub fn parse_project_config(text: &str) -> Result<ProjectConfig> {
    let value = jsonc_parser::parse_to_serde_value(text, &ParseOptions::default())
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    let Some(value) = value else {
        // An empty / comment-only file is a valid empty config.
        return Ok(ProjectConfig::default());
    };
    let obj = as_object(&value, "")?;
    validate_root(obj)
}

/// The PRODUCTION entry — the only caller that consults the gate. Off ⇒ `Ok(None)`
/// (no file discovered or read; effective config = global only). On ⇒ discover
/// up-tree from `start` and read; a malformed project file propagates as an error
/// (fail-loud), a genuinely absent file is `Ok(None)`.
pub fn load_project_config(start: &Path) -> Result<Option<ProjectConfig>> {
    if !discovery_enabled() {
        return Ok(None);
    }
    match discover_project_config(start) {
        Some(path) => read_project_config_at(&path).map(Some),
        None => Ok(None),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation. Hand-walk the serde value so every object level can reject unknown
// keys (serde's `deny_unknown_fields` can't express the per-axis raw-value forms
// or the trichotomy, and loses the JSON-path for messages).
// ─────────────────────────────────────────────────────────────────────────────

/// Join a parent JSON path with a child key (`""` at the root ⇒ bare key).
fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn as_object<'a>(v: &'a Value, path: &str) -> Result<&'a serde_json::Map<String, Value>> {
    v.as_object().ok_or_else(|| ConfigError::Type {
        path: if path.is_empty() {
            "<root>".into()
        } else {
            path.into()
        },
        expected: "an object",
    })
}

fn as_bool(v: &Value, path: &str) -> Result<bool> {
    v.as_bool().ok_or_else(|| ConfigError::Type {
        path: path.into(),
        expected: "a boolean",
    })
}

fn as_str<'a>(v: &'a Value, path: &str) -> Result<&'a str> {
    v.as_str().ok_or_else(|| ConfigError::Type {
        path: path.into(),
        expected: "a string",
    })
}

/// A `string[]` field — every element must be a string.
fn as_string_array(v: &Value, path: &str) -> Result<Vec<String>> {
    let arr = v.as_array().ok_or_else(|| ConfigError::Type {
        path: path.into(),
        expected: "an array of strings",
    })?;
    arr.iter()
        .map(|e| as_str(e, path).map(str::to_string))
        .collect()
}

/// A `{ string: string }` map (define, loader) — every value must be a string.
fn as_string_map(v: &Value, path: &str) -> Result<BTreeMap<String, String>> {
    let obj = as_object(v, path)?;
    obj.iter()
        .map(|(k, val)| Ok((k.clone(), as_str(val, &child(path, k))?.to_string())))
        .collect()
}

/// Reject any key of `obj` not in `allowed` (fail-loud). The blessed `$schema`
/// key is tolerated at the root only (the caller adds it to `allowed` there).
fn reject_unknown_keys(
    obj: &serde_json::Map<String, Value>,
    path: &str,
    allowed: &[&str],
) -> Result<()> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ConfigError::UnknownKey {
                path: if path.is_empty() {
                    "<root>".into()
                } else {
                    path.into()
                },
                key: key.clone(),
            });
        }
    }
    Ok(())
}

fn validate_root(obj: &serde_json::Map<String, Value>) -> Result<ProjectConfig> {
    const ALLOWED: &[&str] = &[
        "$schema",
        "nodeCompat",
        "preload",
        "nodeOptions",
        "v8Flags",
        "env",
        "define",
        "loader",
        "conditions",
        "tsconfig",
        "verifyDepsBeforeRun",
        "sandbox",
        "install",
        "dlx",
    ];
    reject_unknown_keys(obj, "", ALLOWED)?;

    let mut cfg = ProjectConfig::default();
    if let Some(v) = obj.get("nodeCompat") {
        cfg.node_compat = Some(as_bool(v, "nodeCompat")?);
    }
    if let Some(v) = obj.get("preload") {
        cfg.preload = as_string_array(v, "preload")?;
    }
    if let Some(v) = obj.get("nodeOptions") {
        cfg.node_options = as_string_array(v, "nodeOptions")?;
    }
    if let Some(v) = obj.get("v8Flags") {
        cfg.v8_flags = as_string_array(v, "v8Flags")?;
    }
    if let Some(v) = obj.get("env") {
        cfg.env = Some(validate_env_setting(v, "env")?);
    }
    if let Some(v) = obj.get("define") {
        cfg.define = as_string_map(v, "define")?;
    }
    if let Some(v) = obj.get("loader") {
        cfg.loader = as_string_map(v, "loader")?;
    }
    if let Some(v) = obj.get("conditions") {
        cfg.conditions = as_string_array(v, "conditions")?;
    }
    if let Some(v) = obj.get("tsconfig") {
        cfg.tsconfig = Some(as_str(v, "tsconfig")?.to_string());
    }
    if let Some(v) = obj.get("verifyDepsBeforeRun") {
        cfg.verify_deps_before_run = Some(validate_verify_deps(v, "verifyDepsBeforeRun")?);
    }
    if let Some(v) = obj.get("sandbox") {
        cfg.sandbox = Some(validate_sandbox(v, "sandbox")?);
    }
    if let Some(v) = obj.get("install") {
        cfg.install = validate_install(v, "install")?;
    }
    if let Some(v) = obj.get("dlx") {
        cfg.dlx = validate_dlx(v, "dlx")?;
    }
    Ok(cfg)
}

/// `env` / `dlx.env`: `true` | `false` | string | string[].
fn validate_env_setting(v: &Value, path: &str) -> Result<EnvSetting> {
    match v {
        Value::Bool(true) => Ok(EnvSetting::Default),
        Value::Bool(false) => Ok(EnvSetting::Disabled),
        Value::String(s) => Ok(EnvSetting::Sources(vec![s.clone()])),
        Value::Array(_) => Ok(EnvSetting::Sources(as_string_array(v, path)?)),
        _ => Err(ConfigError::Type {
            path: path.into(),
            expected: "a boolean, string, or array of strings",
        }),
    }
}

fn validate_verify_deps(v: &Value, path: &str) -> Result<VerifyDepsBeforeRun> {
    match v {
        Value::Bool(b) => Ok(VerifyDepsBeforeRun::Enabled(*b)),
        Value::String(s) => match s.as_str() {
            "install" => Ok(VerifyDepsBeforeRun::Install),
            "warn" => Ok(VerifyDepsBeforeRun::Warn),
            "error" => Ok(VerifyDepsBeforeRun::Error),
            "prompt" => Ok(VerifyDepsBeforeRun::Prompt),
            other => Err(ConfigError::Value {
                path: path.into(),
                message: format!(
                    "unknown value `{other}` (expected \"install\", \"warn\", \"error\", or \"prompt\")"
                ),
            }),
        },
        _ => Err(ConfigError::Type {
            path: path.into(),
            expected: "a boolean or one of \"install\"/\"warn\"/\"error\"/\"prompt\"",
        }),
    }
}

fn validate_install(v: &Value, path: &str) -> Result<InstallConfig> {
    const ALLOWED: &[&str] = &[
        "nodeLinker",
        "symlinkDisablePattern",
        "hoist",
        "minimumReleaseAge",
        "minimumReleaseAgeExclude",
        "nodeOptions",
        "sandbox",
    ];
    let obj = as_object(v, path)?;
    reject_unknown_keys(obj, path, ALLOWED)?;

    let mut install = InstallConfig::default();
    if let Some(v) = obj.get("nodeLinker") {
        let p = child(path, "nodeLinker");
        install.node_linker = Some(match as_str(v, &p)? {
            "symlink" => NodeLinker::Symlink,
            "isolated" => NodeLinker::Isolated,
            "hoisted" => NodeLinker::Hoisted,
            "pnp" => NodeLinker::Pnp,
            other => {
                return Err(ConfigError::Value {
                    path: p,
                    message: format!(
                        "unknown linker `{other}` (expected \"symlink\", \"isolated\", \"hoisted\", or \"pnp\")"
                    ),
                });
            }
        });
    }
    if let Some(v) = obj.get("symlinkDisablePattern") {
        install.symlink_disable_pattern =
            as_string_array(v, &child(path, "symlinkDisablePattern"))?;
    }
    if let Some(v) = obj.get("hoist") {
        let p = child(path, "hoist");
        install.hoist = Some(match v {
            Value::Bool(b) => Hoist::Bool(*b),
            Value::Array(_) => Hoist::Patterns(as_string_array(v, &p)?),
            _ => {
                return Err(ConfigError::Type {
                    path: p,
                    expected: "a boolean or array of strings",
                });
            }
        });
    }
    if let Some(v) = obj.get("minimumReleaseAge") {
        install.minimum_release_age = Some(parse_duration(
            as_str(v, &child(path, "minimumReleaseAge"))?,
            &child(path, "minimumReleaseAge"),
        )?);
    }
    if let Some(v) = obj.get("minimumReleaseAgeExclude") {
        install.minimum_release_age_exclude =
            as_string_array(v, &child(path, "minimumReleaseAgeExclude"))?;
    }
    if let Some(v) = obj.get("nodeOptions") {
        install.node_options = as_string_array(v, &child(path, "nodeOptions"))?;
    }
    if let Some(v) = obj.get("sandbox") {
        install.sandbox = Some(validate_sandbox(v, &child(path, "sandbox"))?);
    }
    Ok(install)
}

fn validate_dlx(v: &Value, path: &str) -> Result<DlxConfig> {
    const ALLOWED: &[&str] = &["consent", "sandbox", "env"];
    let obj = as_object(v, path)?;
    reject_unknown_keys(obj, path, ALLOWED)?;

    let mut dlx = DlxConfig::default();
    if let Some(v) = obj.get("consent") {
        let p = child(path, "consent");
        let s = as_str(v, &p)?;
        dlx.consent = Some(ImplicitDlx::parse(s).ok_or_else(|| ConfigError::Value {
            path: p,
            message: format!("unknown value `{s}` (expected \"prompt\" or \"never\")"),
        })?);
    }
    if let Some(v) = obj.get("sandbox") {
        dlx.sandbox = Some(validate_sandbox(v, &child(path, "sandbox"))?);
    }
    if let Some(v) = obj.get("env") {
        dlx.env = Some(validate_env_setting(v, &child(path, "env"))?);
    }
    Ok(dlx)
}

/// The `sandbox` trichotomy skeleton. Shape only — the Phase-1 compiler resolves
/// presets/spread/grammar; here we just classify the wrapper and validate axis
/// forms so the schema is right.
fn validate_sandbox(v: &Value, path: &str) -> Result<SandboxSetting> {
    match v {
        Value::Bool(true) => Ok(SandboxSetting::Enabled),
        Value::Bool(false) => Ok(SandboxSetting::Disabled),
        Value::String(s) => Ok(classify_sandbox_string(s)),
        Value::Object(_) => Ok(SandboxSetting::Granular(validate_sandbox_axes(v, path)?)),
        _ => Err(ConfigError::Type {
            path: path.into(),
            expected: "a boolean, string (preset or \"./file.json\"), or object",
        }),
    }
}

/// Preset (bare identifier) vs file-ref (path-like) — the disambiguation rule
/// from the config spec. Unknown-preset validation is the compiler's job (it owns
/// the closed preset vocabulary), so the skeleton only classifies the string.
fn classify_sandbox_string(s: &str) -> SandboxSetting {
    let path_like = s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
        || s.starts_with('~')
        || Path::new(s).extension().is_some();
    if path_like {
        SandboxSetting::FileRef(s.to_string())
    } else {
        SandboxSetting::Preset(s.to_string())
    }
}

fn validate_sandbox_axes(v: &Value, path: &str) -> Result<SandboxAxes> {
    const ALLOWED: &[&str] = &["fs", "net", "env"];
    let obj = as_object(v, path)?;
    reject_unknown_keys(obj, path, ALLOWED)?;

    let mut axes = SandboxAxes::default();
    if let Some(v) = obj.get("fs") {
        axes.fs = Some(validate_sandbox_axis(v, &child(path, "fs"))?);
    }
    if let Some(v) = obj.get("net") {
        axes.net = Some(validate_sandbox_axis(v, &child(path, "net"))?);
    }
    if let Some(v) = obj.get("env") {
        axes.env = Some(validate_sandbox_axis(v, &child(path, "env"))?);
    }
    Ok(axes)
}

fn validate_sandbox_axis(v: &Value, path: &str) -> Result<SandboxAxis> {
    match v {
        Value::Bool(b) => Ok(SandboxAxis::Bool(*b)),
        Value::Array(_) => Ok(SandboxAxis::Array(as_string_array(v, path)?)),
        Value::Object(obj) => Ok(SandboxAxis::Object(
            obj.iter()
                .map(|(k, val)| (k.clone(), val.clone()))
                .collect(),
        )),
        _ => Err(ConfigError::Type {
            path: path.into(),
            expected: "a boolean, array, or object",
        }),
    }
}

/// Parse the strict `minimumReleaseAge` duration grammar: `<integer><unit>`,
/// units `s|m|h|d|w` ONLY (no months/years — calendar ambiguity; `m` is
/// unambiguously minutes). A bare unit-less number is REJECTED (the npm-days vs
/// pnpm-minutes trap, made unrepresentable).
fn parse_duration(s: &str, path: &str) -> Result<Duration> {
    let invalid = |msg: &str| ConfigError::Value {
        path: path.into(),
        message: format!("invalid duration `{s}` — {msg}"),
    };
    let Some(unit) = s.chars().last() else {
        return Err(invalid("empty"));
    };
    let per_unit = match unit {
        's' => 1u64,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 604_800,
        _ => {
            return Err(invalid(
                "expected an integer followed by a unit s|m|h|d|w (e.g. \"3d\")",
            ));
        }
    };
    let digits = &s[..s.len() - unit.len_utf8()];
    if digits.is_empty() {
        return Err(invalid("missing the integer amount"));
    }
    let amount: u64 = digits
        .parse()
        .map_err(|_| invalid("the amount must be a non-negative integer"))?;
    per_unit
        .checked_mul(amount)
        .map(Duration::from_secs)
        .ok_or_else(|| invalid("overflows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ProjectConfig {
        parse_project_config(text).expect("valid config")
    }

    #[test]
    fn empty_and_comment_only_files_are_valid_empty_configs() {
        assert_eq!(parse(""), ProjectConfig::default());
        assert_eq!(parse("// just a comment\n"), ProjectConfig::default());
        assert_eq!(parse("{}"), ProjectConfig::default());
    }

    #[test]
    fn schema_key_is_accepted_and_ignored() {
        let cfg = parse("{ \"$schema\": \"https://nubjs.com/schema/nub.json\" }");
        assert_eq!(cfg, ProjectConfig::default());
    }

    #[test]
    fn unknown_top_level_key_fails_loud() {
        let err = parse_project_config("{ \"nodeComapt\": true }").unwrap_err();
        match err {
            ConfigError::UnknownKey { key, .. } => assert_eq!(key, "nodeComapt"),
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn unknown_nested_key_reports_its_path() {
        let err =
            parse_project_config("{ \"install\": { \"nodeLinkr\": \"symlink\" } }").unwrap_err();
        match err {
            ConfigError::UnknownKey { path, key } => {
                assert_eq!(path, "install");
                assert_eq!(key, "nodeLinkr");
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn malformed_jsonc_is_a_parse_error_not_a_degrade() {
        assert!(matches!(
            parse_project_config("{ this is not json"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn runtime_top_levels_parse() {
        let cfg = parse(
            r#"{
              // jsonc: comments + trailing commas
              "nodeCompat": true,
              "preload": ["./telemetry.ts"],
              "nodeOptions": ["--max-old-space-size=4096"],
              "v8Flags": ["--expose-gc"],
              "define": { "__DEV__": "false" },
              "loader": { ".svg": "text" },
              "conditions": ["worker"],
              "tsconfig": "./tsconfig.runtime.json",
            }"#,
        );
        assert_eq!(cfg.node_compat, Some(true));
        assert_eq!(cfg.preload, vec!["./telemetry.ts"]);
        assert_eq!(cfg.node_options, vec!["--max-old-space-size=4096"]);
        assert_eq!(cfg.v8_flags, vec!["--expose-gc"]);
        assert_eq!(cfg.define.get("__DEV__").map(String::as_str), Some("false"));
        assert_eq!(cfg.loader.get(".svg").map(String::as_str), Some("text"));
        assert_eq!(cfg.conditions, vec!["worker"]);
        assert_eq!(cfg.tsconfig.as_deref(), Some("./tsconfig.runtime.json"));
    }

    #[test]
    fn env_tristate_covers_all_arms() {
        assert_eq!(parse(r#"{ "env": true }"#).env, Some(EnvSetting::Default));
        assert_eq!(parse(r#"{ "env": false }"#).env, Some(EnvSetting::Disabled));
        assert_eq!(
            parse(r#"{ "env": ".env.local" }"#).env,
            Some(EnvSetting::Sources(vec![".env.local".into()]))
        );
        assert_eq!(
            parse(r#"{ "env": [".env", ".env.local"] }"#).env,
            Some(EnvSetting::Sources(vec![
                ".env".into(),
                ".env.local".into()
            ]))
        );
    }

    #[test]
    fn wrong_type_reports_the_field_and_expectation() {
        let err = parse_project_config(r#"{ "preload": "single" }"#).unwrap_err();
        match err {
            ConfigError::Type { path, expected } => {
                assert_eq!(path, "preload");
                assert_eq!(expected, "an array of strings");
            }
            other => panic!("expected Type error, got {other:?}"),
        }
    }

    #[test]
    fn define_rejects_non_string_values() {
        let err = parse_project_config(r#"{ "define": { "__DEV__": false } }"#).unwrap_err();
        match err {
            ConfigError::Type { path, expected } => {
                assert_eq!(path, "define.__DEV__");
                assert_eq!(expected, "a string");
            }
            other => panic!("expected Type error, got {other:?}"),
        }
    }

    #[test]
    fn verify_deps_covers_bool_and_string_arms() {
        assert_eq!(
            parse(r#"{ "verifyDepsBeforeRun": true }"#).verify_deps_before_run,
            Some(VerifyDepsBeforeRun::Enabled(true))
        );
        assert_eq!(
            parse(r#"{ "verifyDepsBeforeRun": "warn" }"#).verify_deps_before_run,
            Some(VerifyDepsBeforeRun::Warn)
        );
        assert!(matches!(
            parse_project_config(r#"{ "verifyDepsBeforeRun": "yes" }"#),
            Err(ConfigError::Value { .. })
        ));
    }

    #[test]
    fn install_block_parses_and_validates() {
        let cfg = parse(
            r#"{
              "install": {
                "nodeLinker": "isolated",
                "symlinkDisablePattern": ["@corp/tool-*"],
                "hoist": ["*types*"],
                "minimumReleaseAge": "3d",
                "minimumReleaseAgeExclude": ["@myorg/*"],
                "nodeOptions": ["--max-old-space-size=2048"]
              }
            }"#,
        );
        assert_eq!(cfg.install.node_linker, Some(NodeLinker::Isolated));
        assert_eq!(cfg.install.symlink_disable_pattern, vec!["@corp/tool-*"]);
        assert_eq!(
            cfg.install.hoist,
            Some(Hoist::Patterns(vec!["*types*".into()]))
        );
        assert_eq!(
            cfg.install.minimum_release_age,
            Some(Duration::from_secs(3 * 86_400))
        );
        assert_eq!(cfg.install.minimum_release_age_exclude, vec!["@myorg/*"]);
        assert_eq!(cfg.install.node_options, vec!["--max-old-space-size=2048"]);
    }

    #[test]
    fn node_linker_rejects_unknown_value() {
        let err =
            parse_project_config(r#"{ "install": { "nodeLinker": "hardlink" } }"#).unwrap_err();
        match err {
            ConfigError::Value { path, .. } => assert_eq!(path, "install.nodeLinker"),
            other => panic!("expected Value error, got {other:?}"),
        }
    }

    #[test]
    fn hoist_bool_and_array_forms() {
        assert_eq!(
            parse(r#"{ "install": { "hoist": false } }"#).install.hoist,
            Some(Hoist::Bool(false))
        );
        assert_eq!(
            parse(r#"{ "install": { "hoist": ["a", "b"] } }"#)
                .install
                .hoist,
            Some(Hoist::Patterns(vec!["a".into(), "b".into()]))
        );
    }

    #[test]
    fn minimum_release_age_grammar() {
        let units = [
            ("30s", 30u64),
            ("5m", 300),
            ("2h", 7200),
            ("3d", 259_200),
            ("1w", 604_800),
        ];
        for (input, secs) in units {
            let cfg = parse(&format!(
                r#"{{ "install": {{ "minimumReleaseAge": "{input}" }} }}"#
            ));
            assert_eq!(
                cfg.install.minimum_release_age,
                Some(Duration::from_secs(secs)),
                "{input}"
            );
        }
        // Bare unit-less numbers are the days-vs-minutes trap — rejected.
        for bad in ["3", "3y", "d", "-1d", "3 d"] {
            assert!(
                matches!(
                    parse_project_config(&format!(
                        r#"{{ "install": {{ "minimumReleaseAge": "{bad}" }} }}"#
                    )),
                    Err(ConfigError::Value { .. })
                ),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn dlx_block_parses() {
        let cfg = parse(
            r#"{
              "dlx": {
                "consent": "never",
                "sandbox": { "net": ["registry.npmjs.org"] },
                "env": false
              }
            }"#,
        );
        assert_eq!(cfg.dlx.consent, Some(ImplicitDlx::Never));
        assert_eq!(cfg.dlx.env, Some(EnvSetting::Disabled));
        assert!(matches!(cfg.dlx.sandbox, Some(SandboxSetting::Granular(_))));
    }

    #[test]
    fn dlx_consent_rejects_unknown_value() {
        assert!(matches!(
            parse_project_config(r#"{ "dlx": { "consent": "always" } }"#),
            Err(ConfigError::Value { .. })
        ));
    }

    #[test]
    fn sandbox_trichotomy_classifies_every_form() {
        assert_eq!(
            parse(r#"{ "sandbox": false }"#).sandbox,
            Some(SandboxSetting::Disabled)
        );
        assert_eq!(
            parse(r#"{ "sandbox": true }"#).sandbox,
            Some(SandboxSetting::Enabled)
        );
        assert_eq!(
            parse(r#"{ "sandbox": "build-jail" }"#).sandbox,
            Some(SandboxSetting::Preset("build-jail".into()))
        );
        assert_eq!(
            parse(r#"{ "sandbox": "./policy.json" }"#).sandbox,
            Some(SandboxSetting::FileRef("./policy.json".into()))
        );
        assert!(matches!(
            parse(r#"{ "sandbox": { "fs": {} } }"#).sandbox,
            Some(SandboxSetting::Granular(_))
        ));
    }

    #[test]
    fn sandbox_axes_accept_array_and_object_forms() {
        let cfg = parse(
            r#"{
              "sandbox": {
                "env": ["NODE_ENV", "VITE_*", "!*_TOKEN"],
                "fs": { "./data": "rw", "~/.ssh": false },
                "net": { "*.sentry.io": true, "*": false }
              }
            }"#,
        );
        let Some(SandboxSetting::Granular(axes)) = cfg.sandbox else {
            panic!("expected granular sandbox");
        };
        assert_eq!(
            axes.env,
            Some(SandboxAxis::Array(vec![
                "NODE_ENV".into(),
                "VITE_*".into(),
                "!*_TOKEN".into()
            ]))
        );
        assert!(matches!(axes.fs, Some(SandboxAxis::Object(_))));
        assert!(matches!(axes.net, Some(SandboxAxis::Object(_))));
    }

    #[test]
    fn sandbox_axis_array_rejects_non_strings() {
        let err = parse_project_config(r#"{ "sandbox": { "env": ["OK", 5] } }"#).unwrap_err();
        assert!(matches!(err, ConfigError::Type { .. }));
    }

    #[test]
    fn unknown_sandbox_axis_fails_loud() {
        let err = parse_project_config(r#"{ "sandbox": { "disk": true } }"#).unwrap_err();
        match err {
            ConfigError::UnknownKey { path, key } => {
                assert_eq!(path, "sandbox");
                assert_eq!(key, "disk");
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    // ── the discovery gate ──

    #[test]
    fn discover_walks_up_tree_to_first_nub_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(FILE_NAME), "{}").unwrap();
        let deep = root.join("packages").join("app").join("src");
        std::fs::create_dir_all(&deep).unwrap();

        let found = discover_project_config(&deep).expect("walks up to the root file");
        assert_eq!(found, root.join(FILE_NAME));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(discover_project_config(dir.path()), None);
    }

    #[test]
    fn gate_off_never_reads_a_present_file() {
        // Default (production) gate: a real, VALID file present up-tree is still
        // not read — load returns None, preserving global-only behavior.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), r#"{ "nodeCompat": true }"#).unwrap();
        assert_eq!(load_project_config(dir.path()).unwrap(), None);
    }

    #[test]
    fn gate_off_never_surfaces_a_malformed_file() {
        // Fail-loud is gated too: with discovery off, even a broken file is inert.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert_eq!(load_project_config(dir.path()).unwrap(), None);
    }

    #[test]
    fn gate_on_reads_the_discovered_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), r#"{ "nodeCompat": true }"#).unwrap();
        let cfg = with_project_config_enabled(|| load_project_config(dir.path()).unwrap());
        assert_eq!(cfg.unwrap().node_compat, Some(true));
    }

    #[test]
    fn gate_on_propagates_a_malformed_file_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        let result = with_project_config_enabled(|| load_project_config(dir.path()));
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn gate_on_with_no_file_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = with_project_config_enabled(|| load_project_config(dir.path()).unwrap());
        assert_eq!(cfg, None);
    }
}
