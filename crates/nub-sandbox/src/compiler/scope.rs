//! Scope resolution — pick the applicable `sandbox` surface for a run from the
//! per-scope homes, most-specific over the phase default.
//!
//! The frontend-less `--sandbox <file.json>` entry passes ONE explicit block and
//! bypasses this entirely; scope resolution is the seam the future project-config
//! frontend (nub.jsonc + package.json metas) plugs into. Implemented + tested here
//! so that frontend lands cheaply, per design.md §2.2 step 1.

use serde_json::Value;

/// A candidate scope, ordered least- to most-specific by construction: the caller
/// passes them in increasing specificity and the most-specific PRESENT one wins.
#[derive(Debug, Clone)]
pub struct ScopeCandidate<'a> {
    /// A human label for diagnostics (e.g. "nub.jsonc sandbox", "scriptsMeta.dev").
    pub label: &'a str,
    /// The surface value at this scope, if present.
    pub value: Option<&'a Value>,
}

/// Choose the most-specific present scope. Returns `(label, value)` or `None`
/// when no scope defines a policy (→ the caller's built-in default / unjailed).
pub fn resolve<'a>(candidates: &[ScopeCandidate<'a>]) -> Option<(&'a str, &'a Value)> {
    candidates
        .iter()
        .rev()
        .find_map(|c| c.value.map(|v| (c.label, v)))
}
