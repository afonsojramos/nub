//! Process-global sink for age-gated (immature) fallback version picks.
//!
//! With `minimumReleaseAge` set in loose mode (the default), the resolver falls
//! back to the lowest satisfying version when every candidate is younger than
//! the cutoff. pnpm does the same, then auto-persists that package to
//! `minimumReleaseAgeExclude` so a later verify-lockfile pass accepts the pin.
//! The resolver here does the fallback but assigns it no policy; an embedder
//! that wants pnpm's auto-persist ARMS this sink before a resolve, DRAINS it
//! after, and writes the exclude entry in whatever config surface its identity
//! model dictates.
//!
//! Disarmed by default: standalone aube never arms it, records nothing, and its
//! behavior is unchanged. The record check is a single relaxed atomic load,
//! reached only on an actual immature pick.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static PICKS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Arm collection for the next resolve, clearing any prior picks. The embedder
/// calls this before an install/add/update resolve it wants to auto-persist.
pub fn arm_age_gated_fallback_pick_collection() {
    if let Ok(mut picks) = PICKS.lock() {
        picks.clear();
    }
    ARMED.store(true, Ordering::Release);
}

/// Record an immature fallback pick as `(registry_name, version)`. No-op unless
/// armed — the standalone-aube path returns after one relaxed atomic load.
pub fn record_age_gated_fallback_pick(name: &str, version: &str) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut picks) = PICKS.lock() {
        picks.push((name.to_string(), version.to_string()));
    }
}

/// Disarm and drain every recorded pick, in record order (the caller dedups).
/// The embedder calls this once, after the resolve completes.
pub fn take_age_gated_fallback_picks() -> Vec<(String, String)> {
    ARMED.store(false, Ordering::Release);
    match PICKS.lock() {
        Ok(mut picks) => std::mem::take(&mut *picks),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Single test: the sink is process-global, so exercising the whole
    // disarmed → armed → drained lifecycle in one test avoids cross-test races.
    #[test]
    fn lifecycle_disarmed_is_noop_armed_collects_take_drains_and_disarms() {
        // Disarmed by default: records are dropped.
        record_age_gated_fallback_pick("a", "1.0.0");
        assert!(take_age_gated_fallback_picks().is_empty());

        // Armed: records land, in order; take drains them.
        arm_age_gated_fallback_pick_collection();
        record_age_gated_fallback_pick("caniuse-lite", "1.0.30001700");
        record_age_gated_fallback_pick("vite", "6.0.0");
        assert_eq!(
            take_age_gated_fallback_picks(),
            vec![
                ("caniuse-lite".to_string(), "1.0.30001700".to_string()),
                ("vite".to_string(), "6.0.0".to_string()),
            ]
        );

        // take disarmed the sink: subsequent records are dropped again.
        record_age_gated_fallback_pick("b", "2.0.0");
        assert!(take_age_gated_fallback_picks().is_empty());
    }
}
