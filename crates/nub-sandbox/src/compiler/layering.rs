//! Tighten-only layering across trust tiers (T0 CLI ∩ T1 user-global ∩ T2
//! project). A lower-trust layer may only ADD restrictions, never widen — a
//! cloned untrusted repo's config is a ratchet that only tightens, the agent-
//! first defense (`.fray/sandbox.md` "Layering / precedence").
//!
//! Modeled at the DECISION level (a composite over the per-layer IRs), not as a
//! single merged IR: intersecting two last-match-wins rulesets into one ordered
//! list is not generally possible without a composite, and the frontend-less
//! `--sandbox` entry is single-term (never layered). The future project frontend
//! consumes this evaluator directly; a single-IR merge for the OS backends is
//! deferred to when those backends + that frontend land together.

use crate::matcher::path::PathMatcher;
use crate::policy::{Effect, FsPolicy};
use std::path::Path;

/// A composite fs decision over ordered layers (most-trusted first is NOT
/// required — intersection is commutative). A path is readable/writable only if
/// EVERY layer permits it (tighten-only): the least-permissive layer wins.
pub struct LayeredFs {
    layers: Vec<PathMatcher>,
}

impl LayeredFs {
    pub fn new(policies: &[&FsPolicy]) -> Self {
        Self {
            layers: policies
                .iter()
                .map(|p| PathMatcher::new(&p.rules))
                .collect(),
        }
    }

    /// Readable iff every layer allows the path (any access).
    pub fn readable(&self, path: &Path) -> bool {
        self.layers
            .iter()
            .all(|m| matches!(m.decide(path).effect, Effect::Allow))
    }

    /// Writable iff every layer grants ReadWrite at the path.
    pub fn writable(&self, path: &Path) -> bool {
        self.layers.iter().all(|m| {
            let d = m.decide(path);
            matches!(d.effect, Effect::Allow)
                && matches!(d.access, crate::policy::FsAccess::ReadWrite)
        })
    }
}

/// `--config` REPLACES the project term (T2) wholesale, but the user-global floor
/// (T1) still applies — a handed-to-you/CI profile must not pierce the machine
/// owner's floor. Given the ordered terms `[T0.., T1, T2]` and an optional
/// `--config` replacement for T2, return the term list to intersect.
pub fn select_terms<'a, T: Clone>(
    higher: &'a [T],
    user_global_floor: Option<&'a T>,
    project: Option<&'a T>,
    config_override: Option<&'a T>,
) -> Vec<&'a T> {
    let mut out: Vec<&T> = higher.iter().collect();
    if let Some(floor) = user_global_floor {
        out.push(floor);
    }
    // --config swaps the project term; the floor above still stands.
    if let Some(t) = config_override.or(project) {
        out.push(t);
    }
    out
}
