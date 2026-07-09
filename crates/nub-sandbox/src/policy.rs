//! The resolved sandbox policy IR (`SandboxPolicy`).
//!
//! This is the compile target (Boundary A): fully RESOLVED plain data with NO
//! residual surface syntax — no presets, no `"..."` spread, no glob-of-globs, no
//! inheritance tokens. The compiler discharges all of that; a backend consumes
//! ONLY the IR and is a pure `IR → OS-primitive` translator.
//!
//! Every type is `serde`-round-trippable. That is a hard requirement: the
//! conformance fixtures assert against a serialized IR, and `--sandbox` can dump
//! it for debugging. Field/entry order is deterministic (`Vec` preserves order,
//! `constructed` is a `BTreeMap`) so snapshots are stable across the matrix.
//!
//! Evaluation model, uniform across the fs/net axes: an ordered entry list plus a
//! `default_effect` base. `decide()` walks the entries and the LAST match wins;
//! nothing matching falls back to `default_effect`. There is no magic floor and
//! no deny-priority (per .fray/sandbox.md "Pure last-match-wins") — the built-in
//! secret denies the compiler injects are ordinary entries subject to the same
//! rule, so a later user allow can override one by ordering.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One resolved policy for one spawned process. Every axis composes
/// independently. Produced by [`crate::compile`], consumed by [`crate::apply`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub fs: FsPolicy,
    pub net: NetPolicy,
    pub env: EnvPolicy,
    pub pid: PidPolicy,
}

/// Allow or Deny — the verdict of a single rule and the base of a ruleset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Allow,
    Deny,
}

// ── filesystem ───────────────────────────────────────────────────────────────

/// Filesystem confinement: ONE ordered last-match-wins ruleset (each Allow
/// carrying its access) plus the tmp posture.
///
/// Provenance: design.md §2.1 sketches parallel `read`/`write` rulesets, but a
/// single ruleset with per-Allow access is strictly more faithful to last-match-
/// wins and removes the "which list does `"..."` splice into" ambiguity. The
/// read-generous/write-tight posture falls out naturally: secure defaults are
/// `[Allow ** access=read, Deny <secrets>]` (everything readable but the secret
/// set, nothing writable), and a `"./data": "rw"` grant appends
/// `Allow ./data access=readwrite` — one list, no floor. Backends derive the
/// read-set (Allow with any access) and write-set (Allow with ReadWrite) from it;
/// a Deny removes both read and write at that path. "No write-without-read" is
/// structural — [`FsAccess`] has no write-only variant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FsPolicy {
    pub rules: FsRuleSet,
    pub tmp: TmpMode,
}

/// Throwaway-tmp handling for the sandboxed child.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TmpMode {
    /// The host tmp is visible (default until a backend tightens it).
    #[default]
    Shared,
    /// A private per-run tmp is mounted; the host tmp is hidden.
    Private,
    /// No tmp access at all.
    Deny,
}

/// An ordered fs ruleset evaluated last-match-wins over a `default_effect` base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsRuleSet {
    pub entries: Vec<FsRule>,
    pub default_effect: Effect,
}

impl Default for FsRuleSet {
    fn default() -> Self {
        // Fail-closed base: an empty ruleset denies everything.
        Self {
            entries: Vec::new(),
            default_effect: Effect::Deny,
        }
    }
}

/// One fs rule: a canonicalized glob, its effect, and (for an Allow) the access
/// it grants. A Deny carries no access. Write-without-read is deliberately
/// unrepresentable — the surface has no `"w"` ladder value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsRule {
    pub matcher: CanonGlob,
    pub effect: Effect,
    pub access: FsAccess,
}

/// The access an fs Allow grants. On the `write` ruleset a `ReadWrite` allow is
/// the write grant; `Read` on `write` is inert (no write). Modeled per-axis so
/// one surface entry (`"./data": "rw"`) can seed both rulesets consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsAccess {
    Read,
    ReadWrite,
}

// ── network ──────────────────────────────────────────────────────────────────

/// Network confinement. `enforce = false` means "no net restriction" (the
/// wrapper/axis `true` case). When enforcing, `rules` is an ordered last-match-
/// wins list the egress proxy (S6) evaluates by SNI/IP; the base is deny-all.
///
/// Provenance: design.md §2.1 sketches `allow_hosts`/`allow_cidrs` allow-lists.
/// The IR keeps a single ordered `rules` list instead so `!`-deny + last-match-
/// wins compose on the net axis exactly as they do on fs — an allow-list can't
/// express `["*", "!*.evil.com"]` faithfully. `admits()` gives the proxy the
/// resolved allow set when it needs a flat view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetPolicy {
    pub enforce: bool,
    pub rules: Vec<NetRule>,
    pub default_effect: Effect,
}

impl Default for NetPolicy {
    fn default() -> Self {
        // Off by default: no rules, not enforcing. The compiler flips `enforce`
        // on for any explicit net policy.
        Self {
            enforce: false,
            rules: Vec::new(),
            default_effect: Effect::Deny,
        }
    }
}

/// One net rule: a host pattern or a CIDR, plus its effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetRule {
    pub target: NetTarget,
    pub effect: Effect,
}

/// A net rule targets either a host pattern (glob or literal) or a CIDR block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetTarget {
    /// A hostname pattern. `*.example.com` matches the apex AND any-depth
    /// subdomains (a deliberate divergence from TLS's one-label wildcard, chosen
    /// for fewer footguns — see .fray/sandbox.md matcher spec).
    Host(String),
    /// A CIDR block for IP-literal egress.
    Cidr(ipnet::IpNet),
}

// ── environment ──────────────────────────────────────────────────────────────

/// Environment confinement. `constructed` is the ACTUAL child env nub builds —
/// env access is undetectable (a plain memory read of the populated environ), so
/// enforcement is construction, not interception: a withheld var is simply absent.
/// `schema` carries per-key validation + secret/public marks for downstream
/// consumers (log redaction); the `$(…)` resolver's output is already baked into
/// `constructed` by the compiler.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvPolicy {
    /// When `false` the child INHERITS the ambient env untouched (no confinement —
    /// the unconfined / absent-axis case). When `true` the child env is EXACTLY
    /// `constructed` — the scrub is construction, not subtraction.
    pub enforce: bool,
    pub constructed: BTreeMap<String, String>,
    pub schema: Vec<EnvRule>,
    /// The names the policy deliberately WITHHELD from the child (present in the
    /// ambient env, denied by policy). Surfaced verbatim in a failure hint — nub
    /// knows exactly what it removed. Deterministic (sorted) for stable output.
    pub withheld: Vec<String>,
}

/// A single env-key rule carried for validation + redaction. Enforcement of the
/// value itself is via `constructed`; this is the metadata twin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvRule {
    /// The key or glob key (`VITE_*`) the rule governs.
    pub key: String,
    /// Whether the value is sensitive (default-sensitive unless `public`).
    pub secret: bool,
    /// Optional value type the compiler validated the value against.
    pub format: Option<EnvFormat>,
    /// `true` if the key is optional (object-form trailing `?` / `optional`).
    pub optional: bool,
}

/// The closed env value-type grammar (`integer | number | port`). String formats
/// (email/url/…) deliberately do NOT ship; `/regex/` covers them until real
/// demand (.fray/sandbox-config-spec.md — FORMAT trimmed 2026-07-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvFormat {
    Integer,
    Number,
    Port,
}

// ── pid ──────────────────────────────────────────────────────────────────────

/// PID/isolation posture. `isolate` requests env-read isolation on Linux (§2.4);
/// PID-ns is opportunistic (userns-gated) — the primary env-read boundary is
/// Landlock `/proc` read-confine + seccomp ptrace-deny, not this flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidPolicy {
    pub isolate: bool,
}

// ── canonical glob ───────────────────────────────────────────────────────────

/// A fully-resolved fs glob: symbolic roots (`~`/`<tmp>`/`<home>`/`<cache>`/`./`)
/// already expanded and slashes normalized to `/`. Case-insensitivity is applied
/// at MATCH time (via globset's flag on Windows/macOS), NOT baked here, so the
/// serialized IR is byte-identical across OSes and snapshots stay stable.
/// Serializes as its string; the matcher compiles it to a `globset` matcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonGlob(pub String);

impl CanonGlob {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
