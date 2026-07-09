//! The closed preset table. A `"sandbox": "<preset>"` string opts into a
//! nub-implemented named policy set. The resolver is a CLOSED table — an unknown
//! preset is a hard error naming the supported set (same discipline as the env
//! type grammar), so adding a preset later is non-breaking.
//!
//! A preset expands to the equivalent granular surface `Value`, which the pipeline
//! then folds — one code path, no separate preset→IR translator to keep in sync.

use super::CompileError;
use serde_json::{Value, json};

/// Resolve a preset name to its granular surface object. `"build-jail"` is the
/// only preset today (the lifecycle-script baseline).
pub fn resolve(name: &str) -> Result<Value, CompileError> {
    match name {
        "build-jail" => Ok(build_jail()),
        other => Err(CompileError::unknown_preset(other, &["build-jail"])),
    }
}

/// The build-jail baseline: read everything but the secret set, write only the
/// project subtree, deny all egress, strip the ambient env. Expressed in surface
/// syntax so it folds through the same pipeline as a hand-written policy.
///
/// This is a Stage-1 approximation of the lifecycle-script posture — the
/// production build-jail (own-package-dir confinement, prefetch, interactive net
/// grants, curated env baseline) is the later build-jail thread's job; it refines
/// this via `install.sandbox` + `dependenciesMeta` grants. Kept deliberately
/// simple + project-relative so it is meaningful in the frontend-less engine.
fn build_jail() -> Value {
    json!({
        // generous read minus secrets (`"..."`), plus the project subtree rw.
        "fs": ["...", "./"],
        // deny all egress (the build-jail thread adds prefetch + grants).
        "net": false,
        // strip the ambient env (the embedder injects the curated baseline).
        "env": false
    })
}
