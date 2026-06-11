//! Version-keyed flag injection with three-stage opt-out merging.

use super::version::NodeVersion;

/// Flags Nub injects on EVERY supported Node version. `--enable-source-maps` has
/// existed since Node 12.12, so it is safe across the whole 18.19+ range.
/// (`--disable-warning` is NOT here — it doesn't exist on Node 18.x / 20.0–20.10
/// and is gated below; injecting it there is a hard "bad option" / "not allowed
/// in NODE_OPTIONS" error, which broke the compat tier on those versions.)
const ALWAYS_INJECT: &[&str] = &["--enable-source-maps"];

/// `--disable-warning=ExperimentalWarning` (suppresses Node's experimental-feature
/// warning) was added in Node 21.3.0 and backported to 20.11.0. It does NOT exist
/// on 18.x or 20.0–20.10, where passing it aborts the process ("bad option" as
/// argv, "not allowed in NODE_OPTIONS" via env). Inject it only at/above this
/// floor; below it the (cosmetic) experimental warning is left unsuppressed — far
/// better than refusing to start. Verified against real Node 18.19 / 20.11 / 22.13.
const MIN_DISABLE_WARNING: NodeVersion = NodeVersion::new(20, 11, 0);

/// `--experimental-webstorage` + `--localstorage-file` landed in Node 22.4.0.
/// Below it both are "bad option" — so nub's default-on webstorage must be gated
/// on this floor (the compat tier 18.19–22.3 simply runs without webstorage).
pub const MIN_WEBSTORAGE: NodeVersion = NodeVersion::new(22, 4, 0);

/// `--experimental-webstorage` was unflagged (defaults on) in Node 25.0.0 (PR
/// nodejs/node#57666). At/above this version the `localStorage`/`sessionStorage`
/// globals are installed without the experimental flag — only `--localstorage-file`
/// is still required for them to actually exist (Node 26+ leaves the global
/// undefined, and throws on access, when no file is provided). So nub injects the
/// EXPERIMENTAL flag only on 22.4–24.x; on 25+ it's a no-op alias (still accepted,
/// not a "bad option") and is left out. The `--localstorage-file` path, by
/// contrast, is injected on EVERY supported version >= 22.4 — that's what makes
/// webstorage present and persistent. Verified empirically on Node 26.2.0 and
/// against .repos/node (v27 pre): `--localstorage-file` alone exposes a working,
/// persistent `localStorage`; the flag without the file does not.
pub const MIN_WEBSTORAGE_NATIVE: NodeVersion = NodeVersion::new(25, 0, 0);

/// Whether the target Node has Web Storage at all (the version floor where
/// `--experimental-webstorage` / `--localstorage-file` exist). Callers gate the
/// `--localstorage-file` injection on this so older compat-tier Node isn't handed a
/// flag it rejects. True for every Node >= 22.4 — including 25/26 where the global
/// is native (it still needs the file to materialize).
pub fn webstorage_supported(node_version: &NodeVersion) -> bool {
    *node_version >= MIN_WEBSTORAGE
}

/// Whether nub must inject the `--experimental-webstorage` FLAG (as opposed to just
/// the `--localstorage-file` path). Only on the 22.4–24.x band where the feature is
/// still flag-gated; on 25+ the flag defaults on, so injecting it is unnecessary
/// (and we leave it off to stay close to plain-Node argv).
pub fn webstorage_flag_needed(node_version: &NodeVersion) -> bool {
    *node_version >= MIN_WEBSTORAGE && *node_version < MIN_WEBSTORAGE_NATIVE
}

/// `--test-coverage-exclude=<glob>` landed in Node 22.5.0. Below it the flag does
/// not exist: as argv it's a "bad option", and in NODE_OPTIONS it's "not allowed in
/// NODE_OPTIONS" — either way a hard startup abort. nub injects it to keep its own
/// preloaded runtime/*.mjs out of a user's `--experimental-test-coverage` report,
/// but that exclude MUST be gated on this floor — otherwise every nub invocation on
/// 18.19–22.4 dies before running a line (the NODE_OPTIONS form is unconditional).
/// On the compat tier below 22.5 the exclude is simply skipped: nub's runtime shows
/// up in the (rare) coverage report — a cosmetic aggregate quirk, vastly better than
/// refusing to start. Verified against real Node 18.19 / 20.11 / 22.15.
pub const MIN_TEST_COVERAGE_EXCLUDE: NodeVersion = NodeVersion::new(22, 5, 0);

/// Whether the target Node supports `--test-coverage-exclude` (argv or NODE_OPTIONS).
pub fn test_coverage_exclude_supported(node_version: &NodeVersion) -> bool {
    *node_version >= MIN_TEST_COVERAGE_EXCLUDE
}

/// Experimental flags Nub unflags, keyed to the EXACT Node versions where the
/// flag both (a) exists — passing it elsewhere is a hard "bad option" /
/// "not allowed in NODE_OPTIONS" startup abort — and (b) is still needed (the
/// feature is behind the flag, not yet default-on).
///
/// Each entry carries a list of `[lo, hi)` version bands (inclusive low,
/// exclusive high; `hi: None` means "open-ended to infinity"). A single `min`
/// can't express two of these flags: eventsource has a HOLE on the 21.x line
/// (the flag never shipped there), and sqlite is needed in TWO disjoint bands
/// (the 22.x and 23.x lines unflagged at different points). Over-injecting a
/// still-experimental default-true bool (sqlite/websocket after unflag) is a
/// harmless no-op; UNDER-injecting silently loses the feature; injecting where
/// the flag DOESN'T EXIST crashes the process. The bands below are tuned so the
/// flag is present exactly where it both exists and is required.
///
/// Every band cites its changelog evidence — do not re-derive; correct the
/// citation if you find it wrong.
const UNFLAG_TABLE: &[UnflagEntry] = &[
    UnflagEntry {
        flag: "--experimental-vm-modules",
        // vm.Module / vm.SourceTextModule. Flag added in Node 9.6.0 (#14253) and
        // NEVER unflagged through Node 26 — `vm.Module` stays experimental and the
        // flag is always required. So inject across the ENTIRE supported floor
        // (18.19+). (Previously min:22.15.0 left vm.Module broken on 18.19–22.14.)
        bands: &[(NodeVersion::new(18, 19, 0), None)],
    },
    UnflagEntry {
        flag: "--experimental-eventsource",
        // EventSource global (#51575, "add EventSource Client"). Landed on the 22.x
        // line at 22.3.0 and was backported to the 20.x LTS line at 20.18.0. The
        // 21.x line was already EOL when it landed, so the flag NEVER existed there
        // — injecting it on any 21.x is a "bad option" startup crash. Never unflagged
        // through 26. Injection set: [20.18.0, 21.0.0) ∪ [22.3.0, ∞).
        bands: &[
            (NodeVersion::new(20, 18, 0), Some(NodeVersion::new(21, 0, 0))),
            (NodeVersion::new(22, 3, 0), None),
        ],
    },
    // webstorage is NOT in this static table on purpose. It needs a runtime-computed
    // `--localstorage-file=<path>` injected alongside the flag (the path is keyed on
    // the workspace, so it can't be a &'static str), and the experimental flag is
    // version-BANDED (22.4–24.x only — native on 25+) rather than min-gated. Both
    // pieces live in spawn.rs, gated on `webstorage_supported` / `webstorage_flag_needed`
    // + `compute_localstorage_path`. See wiki/runtime/webstorage-unflag.md.
    UnflagEntry {
        flag: "--experimental-sqlite",
        // node:sqlite. Flag added in Node 22.5.0 (#57752). Module unflagged (default
        // import, no flag needed) at 22.13.0 on the 22.x line and at 23.4.0 on the
        // 23.x line. The flag survives as a default-true bool after unflagging, so
        // over-injecting in the unflagged range would be a harmless no-op — but it
        // doesn't EXIST below 22.5.0, so injecting there crashes. Inject only where
        // the flag both exists and is still required: [22.5.0, 22.13.0) ∪ [23.0.0, 23.4.0).
        bands: &[
            (NodeVersion::new(22, 5, 0), Some(NodeVersion::new(22, 13, 0))),
            (NodeVersion::new(23, 0, 0), Some(NodeVersion::new(23, 4, 0))),
        ],
    },
    UnflagEntry {
        flag: "--experimental-websocket",
        // WebSocket global. Flag-gated on [20.10.0, 22.0.0) — the global exists on
        // 20.10+ and all of the 21.x line behind `--experimental-websocket`, then
        // becomes default-on at 22.0.0 (the flag persists as a default-true bool but
        // is no longer required; below 20.10.0 it doesn't exist and is a "bad option").
        // The experimental warning emitted on 22.0–22.3 is already silenced by nub's
        // `--disable-warning=ExperimentalWarning` (injected ≥20.11). Injection set:
        // [20.10.0, 22.0.0).
        bands: &[(NodeVersion::new(20, 10, 0), Some(NodeVersion::new(22, 0, 0)))],
    },
];

struct UnflagEntry {
    flag: &'static str,
    /// Inclusive-low / exclusive-high version bands where the flag is injected.
    /// `hi: None` is open-ended (inject from `lo` to infinity). A version is in
    /// the injection set iff it falls in ANY band.
    bands: &'static [(NodeVersion, Option<NodeVersion>)],
}

impl UnflagEntry {
    /// Whether this flag should be injected for `node_version` — true iff the
    /// version lands in any `[lo, hi)` band.
    fn applies_to(&self, node_version: &NodeVersion) -> bool {
        self.bands.iter().any(|(lo, hi)| {
            *node_version >= *lo && hi.as_ref().map_or(true, |hi| node_version < hi)
        })
    }
}

/// Compute the flags Nub should inject for the given Node version,
/// after subtracting any user opt-outs from argv and NODE_OPTIONS.
///
/// `show_warnings`: if true, suppress the `--disable-warning=ExperimentalWarning`
/// injection (Nub's `--show-warnings` flag).
pub fn compute_inject_flags(
    node_version: NodeVersion,
    user_argv: &[String],
    node_options: Option<&str>,
    show_warnings: bool,
) -> Vec<&'static str> {
    // Stage 1: compute the would-inject set.
    let mut flags: Vec<&str> = Vec::new();

    for &flag in ALWAYS_INJECT {
        flags.push(flag);
    }

    // Warning suppression — only where the flag exists (>= 20.11) and the user
    // hasn't asked to see warnings.
    if !show_warnings && node_version >= MIN_DISABLE_WARNING {
        flags.push("--disable-warning=ExperimentalWarning");
    }

    for entry in UNFLAG_TABLE {
        if entry.applies_to(&node_version) {
            flags.push(entry.flag);
        }
    }

    // Stage 2: parse user opt-outs from argv and NODE_OPTIONS.
    let user_negations = collect_negations(user_argv, node_options);

    // Stage 3: subtract.
    flags.retain(|flag| !user_negations.iter().any(|neg| is_negation_of(neg, flag)));

    flags
}

/// Collect all `--no-experimental-*` and other negation flags from
/// the user's argv and NODE_OPTIONS.
fn collect_negations(user_argv: &[String], node_options: Option<&str>) -> Vec<String> {
    let mut negations = Vec::new();

    for arg in user_argv {
        if arg.starts_with("--no-experimental-") || arg.starts_with("--no-enable-") {
            negations.push(arg.clone());
        }
    }

    if let Some(opts) = node_options {
        for token in opts.split_whitespace() {
            if token.starts_with("--no-experimental-") || token.starts_with("--no-enable-") {
                negations.push(token.to_string());
            }
        }
    }

    negations
}

/// Returns true if `negation` negates `flag`.
/// e.g., "--no-experimental-vm-modules" negates "--experimental-vm-modules".
fn is_negation_of(negation: &str, flag: &str) -> bool {
    if let Some(rest) = negation.strip_prefix("--no-") {
        let positive = format!("--{rest}");
        positive == flag
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> NodeVersion {
        NodeVersion::new(major, minor, patch)
    }

    #[test]
    fn always_injects_warning_suppression_and_source_maps() {
        let flags = compute_inject_flags(v(22, 15, 0), &[], None, false);
        assert!(flags.contains(&"--disable-warning=ExperimentalWarning"));
        assert!(flags.contains(&"--enable-source-maps"));
    }

    #[test]
    fn injects_unflag_set_on_22_15() {
        let flags = compute_inject_flags(v(22, 15, 0), &[], None, false);
        assert!(flags.contains(&"--experimental-vm-modules"));
        assert!(flags.contains(&"--experimental-eventsource"));
        // webstorage is NOT in this static set — it needs a runtime-computed
        // --localstorage-file and version-banded flag logic, handled in spawn.rs.
        assert!(!flags.contains(&"--experimental-webstorage"));
        // sqlite is unflagged on 22.13.0+ (the 22.x line), so 22.15 does NOT inject it.
        assert!(!flags.contains(&"--experimental-sqlite"));
    }

    #[test]
    fn vm_modules_injected_across_entire_floor() {
        // vm.Module is never unflagged — inject from the 18.19 floor through 26.x.
        // (Regression: the old min:22.15.0 left vm.Module broken on 18.19–22.14.)
        assert!(compute_inject_flags(v(18, 19, 0), &[], None, false)
            .contains(&"--experimental-vm-modules"));
        assert!(compute_inject_flags(v(26, 2, 0), &[], None, false)
            .contains(&"--experimental-vm-modules"));
    }

    #[test]
    fn eventsource_skips_the_21x_hole() {
        // EventSource landed at 22.3.0 + 20.18.0 backport; never shipped on 21.x.
        // Injecting on 21.x is a "bad option" crash — the highest-stakes boundary here.
        let yes = "--experimental-eventsource";
        assert!(!compute_inject_flags(v(20, 17, 0), &[], None, false).contains(&yes));
        assert!(compute_inject_flags(v(20, 18, 0), &[], None, false).contains(&yes));
        // The hole: must NOT inject anywhere on the 21.x line.
        assert!(
            !compute_inject_flags(v(21, 0, 0), &[], None, false).contains(&yes),
            "must NOT inject --experimental-eventsource on 21.0 (flag never existed there → crash)"
        );
        assert!(!compute_inject_flags(v(22, 2, 0), &[], None, false).contains(&yes));
        assert!(compute_inject_flags(v(22, 3, 0), &[], None, false).contains(&yes));
        assert!(compute_inject_flags(v(26, 2, 0), &[], None, false).contains(&yes));
    }

    #[test]
    fn sqlite_injected_only_in_the_two_flagged_bands() {
        // node:sqlite: flag added 22.5.0, unflagged 22.13.0 (22.x) and 23.4.0 (23.x).
        // Inject only where the flag exists AND is still required.
        let sql = "--experimental-sqlite";
        assert!(!compute_inject_flags(v(22, 4, 0), &[], None, false).contains(&sql)); // flag absent
        assert!(compute_inject_flags(v(22, 5, 0), &[], None, false).contains(&sql)); // band 1 floor
        assert!(compute_inject_flags(v(22, 12, 0), &[], None, false).contains(&sql));
        assert!(!compute_inject_flags(v(22, 13, 0), &[], None, false).contains(&sql)); // unflagged on 22.x
        assert!(compute_inject_flags(v(23, 3, 0), &[], None, false).contains(&sql)); // band 2
        assert!(!compute_inject_flags(v(23, 4, 0), &[], None, false).contains(&sql)); // unflagged on 23.x
        assert!(!compute_inject_flags(v(24, 0, 0), &[], None, false).contains(&sql)); // unflagged everywhere after
    }

    #[test]
    fn websocket_injected_only_on_flag_gated_band() {
        // WebSocket global is flag-gated on [20.10.0, 22.0.0): exists on 20.10+ and all
        // 21.x, default-on from 22.0.0. Below 20.10 the flag doesn't exist ("bad option").
        let ws = "--experimental-websocket";
        assert!(!compute_inject_flags(v(20, 9, 0), &[], None, false).contains(&ws));
        assert!(compute_inject_flags(v(20, 10, 0), &[], None, false).contains(&ws));
        assert!(compute_inject_flags(v(21, 5, 0), &[], None, false).contains(&ws)); // all of 21.x
        assert!(!compute_inject_flags(v(22, 0, 0), &[], None, false).contains(&ws)); // default-on
    }

    #[test]
    fn user_opt_out_via_argv() {
        let argv = vec!["--no-experimental-vm-modules".to_string()];
        let flags = compute_inject_flags(v(22, 15, 0), &argv, None, false);
        assert!(!flags.contains(&"--experimental-vm-modules"));
        // Other flags still present (eventsource is in-band at 22.15).
        assert!(flags.contains(&"--experimental-eventsource"));
    }

    #[test]
    fn user_opt_out_via_node_options() {
        // Use 22.12.0 where sqlite IS injected (first band), so the opt-out is observable.
        let flags = compute_inject_flags(
            v(22, 12, 0),
            &[],
            Some("--no-experimental-sqlite --max-old-space-size=4096"),
            false,
        );
        assert!(!flags.contains(&"--experimental-sqlite"));
        assert!(flags.contains(&"--experimental-vm-modules"));
    }

    #[test]
    fn show_warnings_suppresses_warning_flag() {
        let flags = compute_inject_flags(v(22, 15, 0), &[], None, true);
        assert!(!flags.contains(&"--disable-warning=ExperimentalWarning"));
        assert!(flags.contains(&"--enable-source-maps"));
    }

    #[test]
    fn floor_injects_only_universally_safe_flags() {
        // At 20.0.0: --enable-source-maps and vm-modules (whole-floor) inject, but the
        // version-gated entries do not — sqlite/eventsource/websocket flags don't exist
        // here ("bad option"), and --disable-warning is below its 20.11 floor.
        let flags = compute_inject_flags(v(20, 0, 0), &[], None, false);
        assert!(flags.contains(&"--enable-source-maps"));
        assert!(flags.contains(&"--experimental-vm-modules"));
        assert!(!flags.contains(&"--experimental-sqlite"));
        assert!(!flags.contains(&"--experimental-eventsource"));
        assert!(!flags.contains(&"--experimental-websocket")); // below 20.10 floor
        assert!(!flags.contains(&"--disable-warning=ExperimentalWarning"));
    }

    #[test]
    fn disable_warning_gated_to_node_that_supports_it() {
        // Node 18.19 and 20.0–20.10 reject `--disable-warning` ("bad option" /
        // "not allowed in NODE_OPTIONS"), which crashed the compat tier. It must
        // not be injected below 20.11; from 20.11 onward it is.
        for ver in [v(18, 19, 0), v(20, 0, 0), v(20, 10, 0)] {
            let flags = compute_inject_flags(ver.clone(), &[], None, false);
            assert!(
                !flags.contains(&"--disable-warning=ExperimentalWarning"),
                "must NOT inject --disable-warning on {ver:?} (the flag aborts those versions)"
            );
            // --enable-source-maps is always safe, so the floor still augments.
            assert!(
                flags.contains(&"--enable-source-maps"),
                "source-maps must still inject on {ver:?}"
            );
        }
        for ver in [v(20, 11, 0), v(22, 13, 0)] {
            let flags = compute_inject_flags(ver.clone(), &[], None, false);
            assert!(
                flags.contains(&"--disable-warning=ExperimentalWarning"),
                "must inject --disable-warning on {ver:?} (supported there)"
            );
        }
    }

    #[test]
    fn webstorage_supported_floor_is_22_4() {
        // Below 22.4 the webstorage flags don't exist ("bad option") — so the
        // --localstorage-file injection (and webstorage entirely) is skipped. At/above
        // 22.4 it's supported on EVERY version, including the native 25/26 (the file is
        // still required there for the global to materialize).
        assert!(!webstorage_supported(&v(18, 19, 0)));
        assert!(!webstorage_supported(&v(20, 11, 0)));
        assert!(!webstorage_supported(&v(22, 3, 0)));
        assert!(webstorage_supported(&v(22, 4, 0)));
        assert!(webstorage_supported(&v(22, 13, 0)));
        assert!(webstorage_supported(&v(24, 0, 0)));
        assert!(webstorage_supported(&v(25, 0, 0)));
        assert!(webstorage_supported(&v(26, 2, 0)));
    }

    #[test]
    fn experimental_webstorage_flag_only_needed_on_22_4_through_24() {
        // The --experimental-webstorage FLAG is only needed where the feature is
        // flag-gated. It was unflagged (defaults on) in Node 25.0.0, so on 25+ nub
        // injects only --localstorage-file, not the experimental flag.
        assert!(!webstorage_flag_needed(&v(22, 3, 0))); // flag doesn't exist yet
        assert!(webstorage_flag_needed(&v(22, 4, 0))); // floor: flag needed
        assert!(webstorage_flag_needed(&v(22, 15, 0)));
        assert!(webstorage_flag_needed(&v(24, 0, 0)));
        assert!(webstorage_flag_needed(&v(24, 99, 0))); // still flagged through 24.x
        assert!(!webstorage_flag_needed(&v(25, 0, 0))); // native — flag not needed
        assert!(!webstorage_flag_needed(&v(26, 2, 0)));
    }

    #[test]
    fn test_coverage_exclude_gated_to_22_5() {
        // `--test-coverage-exclude` was added in Node 22.5.0. Below it the flag is
        // rejected in NODE_OPTIONS ("not allowed in NODE_OPTIONS") — and because nub
        // injects it UNCONDITIONALLY whenever a preload is present, an ungated inject
        // aborts EVERY nub invocation on the entire 18.19–22.4 range (the most common
        // LTS lines). Callers must gate on this; this guards the regression.
        assert!(!test_coverage_exclude_supported(&v(18, 19, 0)));
        assert!(!test_coverage_exclude_supported(&v(20, 11, 0)));
        assert!(!test_coverage_exclude_supported(&v(22, 4, 0)));
        assert!(test_coverage_exclude_supported(&v(22, 5, 0)));
        assert!(test_coverage_exclude_supported(&v(22, 15, 0)));
        assert!(test_coverage_exclude_supported(&v(24, 0, 0)));
    }
}
