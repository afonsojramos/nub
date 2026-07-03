//! Install-time progress UI built on top of `clx::progress`.
//!
//! Two modes live behind one API so call sites in `install::run` stay the same:
//!
//! * **TTY** — a single in-place animated line, bun/uv-style: an animated
//!   spinner, the phase verb (right-padded to a fixed width so nothing after
//!   it shifts on a phase change), and a live count — `cur/total pkgs` during
//!   resolve/fetch, a ticking `N files` during linking. NO progress bar (a bar
//!   either lies by freezing at a fixed fill through linking or flashes a
//!   near-full frame on a fast link — see #288); the spinner is proof-of-life
//!   and the count carries the real state. Deliberately *single-line*: no
//!   per-package child rows, so the region never grows/shrinks. A short
//!   debounce keeps the line hidden until the install has run ~300ms, so a fast
//!   install flashes nothing. When the install completes the line is cleared
//!   exactly once and the post-install summary takes its place — a clean
//!   handoff with no leftover frame, matching pnpm / bun / cargo / uv.
//! * **Append-only** — lines safe for terminals, GitHub Actions, and plain
//!   pipes: a single repeating pnpm-style `Progress:` line emitted on a ~2s
//!   heartbeat, showing `resolved` / `reused` / `downloaded` plus the byte
//!   total for the downloaded set. The heartbeat only prints when something
//!   actually advanced, so a fast install stays quiet and a slow one shows
//!   exactly *why* it's slow (network-bound vs linker-bound). No phase noise,
//!   no child rows, no redraws.
//!
//! `try_new` picks the renderer from the active embedder profile and the
//! environment: the in-place TTY renderer when stderr is an interactive,
//! non-CI terminal **and** the embedder opts in (`Embedder::tty_progress`, or
//! the `AUBE_TTY_PROGRESS` env override) — otherwise append-only. CI / piped /
//! non-TTY output is always append-only so a redirected log never carries
//! cursor-control escapes. It returns `None` only when clx has been forced into
//! text mode (`--silent`, `-v`, `--reporter=append-only|ndjson`) — those modes
//! own their own output and we stay out of the way.

mod ci;
mod render;

pub(crate) use render::format_bytes;

use ci::{CiState, format_duration};
use clx::progress::{
    ProgressJob, ProgressJobBuilder, ProgressJobDoneBehavior, ProgressOutput, ProgressStatus,
};
use clx::style;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

/// Denominator the clx `progress_current`/`progress_total` pair is held at in
/// TTY mode. There is no visible template bar — the pair exists only to feed
/// clx's OSC terminal progress indicator (the iTerm2 / VS Code taskbar
/// percentage), which reads `overall_progress()`. Encoding the
/// unified-progress fraction as `progress_current / TTY_BAR_SCALE` gives that
/// indicator 10 000 smooth steps; the user-facing count is owned by the
/// `count` prop.
const TTY_BAR_SCALE: usize = 10_000;

/// The phase verbs the install can emit, in their display spelling. The TTY
/// renderer right-pads the active verb to the widest of these so the bar's `[`
/// column is byte-identical across every phase. This is the single source of
/// truth for the pad width — keep it in sync with `set_phase`'s verb→number
/// map. (Verbs are ASCII, so byte length == display width.)
const TTY_PHASE_VERBS: &[&str] = &["resolving", "fetching", "linking"];

/// Display width every phase verb is right-padded to (the longest
/// [`TTY_PHASE_VERBS`]). Computed at compile time so adding a verb to the list
/// re-derives the pad automatically.
const TTY_PHASE_W: usize = {
    let mut max = 0;
    let mut i = 0;
    while i < TTY_PHASE_VERBS.len() {
        let l = TTY_PHASE_VERBS[i].len();
        if l > max {
            max = l;
        }
        i += 1;
    }
    max
};

/// How long a phase must run before the animated line reveals itself.
/// The body renders empty (via its `{% if revealed %}` guard) until the
/// install has been going this long, so a fast install — resolve, fetch,
/// and link all finishing inside the window — flashes nothing and only
/// its summary prints, matching bun/uv. A slow install blows past this
/// (during network resolve/fetch, or on the reporter's slow link) and
/// the spinner + live count reveal.
const TTY_REVEAL_DELAY: Duration = Duration::from_millis(300);

/// Poll interval of the link-progress ticker thread. During linking the
/// file count comes from an atomic the linker bumps directly (no clx
/// event), so a background ticker re-reads it and repaints the `count`
/// prop on this cadence — fast enough to read as a live tick, slow
/// enough that the repaint cost is negligible against filesystem I/O.
/// It also drives the reveal check so the line appears within one tick
/// of crossing [`TTY_REVEAL_DELAY`] even when no other event fires.
const TTY_TICK_INTERVAL: Duration = Duration::from_millis(90);

/// Digit-field width for the cur/total counters. The numerator is right-aligned
/// and the total left-aligned to this width, so `   7/331 ` and ` 577/623 `
/// occupy an identical column for installs up to 9999 packages; a 5-digit total
/// (10k+ deps, rare) is the only case that nudges the trailing field, and it
/// degrades gracefully (the field just grows by one column).
const TTY_COUNT_DIGITS: usize = 4;

/// Trim `reused` so `reused + downloaded <= total`. No-op when the
/// counters already fit. Called from `set_total` after a downward
/// rebase (post-`filter_graph`) so streamed-then-pruned credits don't
/// leave the numerator above the new denominator.
fn clamp_reused_to(reused: &AtomicUsize, downloaded: &AtomicUsize, total: usize) {
    let dl = downloaded.load(Ordering::Relaxed);
    let cap = total.saturating_sub(dl);
    let _ = reused.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
        (cur > cap).then_some(cap)
    });
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// Whether the vendor attribution (`by jdx.dev`) may render into the banner.
/// The vendor tag is aube's own credit, so it shows only when the active
/// embedder is standalone aube; any other embedder suppresses it, so the
/// engine's vendor brand never leaks into a host's user-facing install output.
/// Keyed on name matching standalone aube's, since `embedder()` returns a
/// copied profile with no stable pointer to compare against `AUBE`. Takes the
/// caller's already-fetched profile so the banner resolves it once.
fn banner_vendor(id: &'static aube_util::Embedder) -> Option<&'static str> {
    if id.name == aube_util::AUBE.name {
        id.vendor
    } else {
        None
    }
}

/// Render the product banner — `<display_name> <VERSION>[ <vendor>]` —
/// used by the install-progress headers and the no-op/fast-mode
/// summaries. The optional vendor attribution is gated by
/// [`banner_vendor`] so the engine's brand never leaks into an
/// embedder's install output. Trailing `suffix` (already styled) is
/// appended verbatim — `" · ✓ msg"` for the summary lines, empty for
/// the bare header.
fn product_banner(suffix: &str) -> String {
    let id = aube_util::embedder();
    match banner_vendor(id) {
        Some(vendor) => format!(
            "{} {} {}{suffix}",
            style::emagenta(id.display_name).bold(),
            style::edim(crate::version::VERSION.as_str()),
            style::edim(vendor),
        ),
        None => format!(
            "{} {}{suffix}",
            style::emagenta(id.display_name).bold(),
            style::edim(crate::version::VERSION.as_str()),
        ),
    }
}

/// Build the standard `<product banner> · <msg>` one-line header used
/// by the no-op and fast-mode summaries. Centralizes the header shape
/// so the install-finished, already-up-to-date, and fast-mode-summary
/// paths all read consistently.
pub(crate) fn aube_prefix_line(msg: &str) -> String {
    product_banner(&format!(" {} {msg}", style::edim("·")))
}

/// Install-time progress UI. Cheap to clone (internally `Arc`).
pub struct InstallProgress {
    mode: Mode,
    /// Per-dep_path `unpacked_size` values captured during streaming
    /// resolve. The running `estimated_bytes` total is the sum, but
    /// `filter_graph` later prunes platform-mismatched optionals from
    /// `graph.packages` — leaving that pruned size still folded into
    /// the estimate would overstate the `~13.8 MB` segment. The post-
    /// `filter_graph` reconcile walks the surviving dep_paths through
    /// this map and resets the estimate to the survivors' sum. `Mutex`
    /// is fine: the streaming pass is the only writer and the
    /// reconcile reads once at the phase boundary.
    unpacked_sizes: Arc<Mutex<HashMap<String, u64>>>,
    /// Shared live file-linking counter. Handed to the linker via
    /// [`link_progress_counter`](InstallProgress::link_progress_counter)
    /// so the materialize pass bumps it once per file; both renderers
    /// read it to show the ticking `N files` count during linking. In CI
    /// mode this is the same `Arc` the `CiState` holds; in TTY mode the
    /// ticker thread reads it.
    files_linked: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum Mode {
    Tty {
        root: Arc<ProgressJob>,
        /// Set after explicit finish so Drop does not later clear the
        /// terminal rows that the success path intentionally preserved.
        finished: Arc<AtomicBool>,
        /// Our own mirror of the denominator so `inc_total` can atomically
        /// fetch-add without racing a concurrent reader/writer through clx's
        /// separate `overall_progress()` / `progress_total()` calls.
        total: Arc<AtomicUsize>,
        /// Resolving-phase denominator hint. Seeded from any lockfile
        /// on disk before resolution starts and raised by the
        /// resolver's BFS-frontier signal during resolution.
        /// `fetch_max` semantics keep it from ever shrinking. Drives
        /// the resolving-slice fill via the shared
        /// [`render::unified_progress`] math — the clx
        /// `{{progress_bar}}` template reads `progress_current` /
        /// `progress_total`, which `refresh_tty_bar` scales to encode
        /// the unified-progress fraction.
        target_total: Arc<AtomicUsize>,
        /// Mirror of cumulative reused-package count so the TTY bar can
        /// recompute the live numerator without taking a round-trip
        /// through clx's progress accessors.
        reused: Arc<AtomicUsize>,
        /// Mirror of cumulative downloaded-package count for the same
        /// reason.
        downloaded: Arc<AtomicUsize>,
        /// Phase number: 0=init, 1=resolving, 2=fetching, 3=linking. Used
        /// by the rate / ETA props to gate display to the fetching
        /// window and switch to the `linking` label in phase 3.
        phase_num: Arc<AtomicUsize>,
        /// Cumulative downloaded bytes. Fed into the transfer-rate
        /// calculation displayed in the TTY bar's `rate` prop.
        downloaded_bytes: Arc<AtomicU64>,
        /// Running sum of `dist.unpackedSize` from packuments seen
        /// during the streaming resolve. `0` on the lockfile fast path.
        /// The bar's `bytes` prop renders `4.2 MB / ~13.8 MB` when this
        /// is set; otherwise just `4.2 MB`.
        estimated_bytes: Arc<AtomicU64>,
        /// Captured the first time `set_phase("fetching")` is called.
        /// Used as the rate denominator so the displayed throughput
        /// measures the fetch window only, not `bytes / (resolve_time +
        /// fetch_time)`.
        fetch_start: Arc<OnceLock<Instant>>,
        /// Snapshot of `reused + downloaded` at the moment
        /// `set_phase("fetching")` first fires. Used as the baseline
        /// for the fetch-window ETA so the displayed estimate
        /// reflects per-package throughput *during fetching*, not the
        /// install-elapsed denominator. `usize::MAX` sentinel = "not
        /// captured yet"; render falls back to `ETA …`.
        completed_at_fetch_start: Arc<AtomicUsize>,
        /// Install start instant — the debounce baseline. The live line
        /// stays hidden (the body's `{% if revealed %}` guard renders
        /// empty) until this is older than [`TTY_REVEAL_DELAY`], so a
        /// sub-debounce install flashes nothing and only its summary
        /// prints. Copy, so the ticker captures it by value.
        start: Instant,
        /// One-shot latch guarding the `revealed` prop flip so it's
        /// pushed to clx exactly once (see [`maybe_reveal`]).
        revealed: Arc<AtomicBool>,
        /// Wakes the link-progress ticker for prompt shutdown at
        /// `finish()` instead of waiting out its poll interval. Its
        /// companion `wake_lock` lives only on the ticker (a `notify`
        /// doesn't need the lock, so the display side never holds it).
        wake: Arc<Condvar>,
        /// Join handle for the link-progress ticker thread, taken and
        /// joined by `finish()` / `Drop` so no late `count` write can
        /// repaint after the display is cleared.
        ticker: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    },
    Ci(Arc<CiState>),
}

impl Clone for InstallProgress {
    /// CI mode tracks its own "alive clones" refcount instead of relying on
    /// `Arc::strong_count`, because the heartbeat thread owns an `Arc<CiState>`
    /// for the entire run and would otherwise pin `strong_count ≥ 2` — defeating
    /// the `== 1` shutdown check in `Drop`.
    fn clone(&self) -> Self {
        if let Mode::Ci(s) = &self.mode {
            s.alive.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            mode: self.mode.clone(),
            unpacked_sizes: self.unpacked_sizes.clone(),
            files_linked: self.files_linked.clone(),
        }
    }
}

impl InstallProgress {
    /// Construct a new install progress UI, or `None` if progress should be
    /// disabled (clx text mode — i.e. `--silent`, `-v`, or a line-oriented
    /// reporter that owns its own output).
    pub fn try_new() -> Option<Self> {
        if clx::progress::output() == ProgressOutput::Text {
            return None;
        }
        // In-place animated bar only on an interactive, non-CI terminal, and
        // only when the active embedder opts in (or the `AUBE_TTY_PROGRESS`
        // override is set). Everything else — CI, a pipe, a redirected log —
        // takes the append-only renderer so no cursor-control escape ever
        // lands in a non-TTY stream. The renderer itself is single-line with a
        // clean progress→summary handoff, so the in-place path is a first-class
        // UX (nub enables it by default) rather than a debug-only fallback.
        let tty_opt_in = aube_util::embedder().tty_progress || env_truthy("AUBE_TTY_PROGRESS");
        if std::io::stderr().is_terminal() && !is_ci::cached() && tty_opt_in {
            Some(Self::new_tty())
        } else {
            Some(Self::new_ci())
        }
    }

    fn new_tty() -> Self {
        // Colored header: magenta bold display name, dim version, dim
        // vendor. Mirrors the `mise VERSION by @jdx` / `hk VERSION by
        // @jdx` convention for visual parity across the trio. The vendor
        // attribution renders only for standalone aube (see
        // `product_banner`); an embedder drops it so the engine brand
        // never leaks into the host's install output.
        let header = product_banner("");
        // Layout — bun/uv-style: an animated spinner, the phase verb, and a
        // live count. NO progress bar (a bar for the linking phase either
        // lies by holding at a fixed fill or flashes a near-full frame on a
        // fast link — see #288); the spinner is proof-of-life on its own and
        // the count carries the real state.
        //
        //   <header>  {{spinner()}} <phase>  <count><bytes><rate><eta>
        //
        // The whole line is wrapped in `{% if revealed %}` so it renders
        // EMPTY (and clx drops the row) until the debounce fires — a fast
        // install flashes nothing. `{{phase}}` is the verb RIGHT-PADDED to
        // the widest verb (`tty_phase_field`), so the `count` column — and
        // everything after it — holds byte-stable when the phase word
        // changes; only the count/spinner move, and they're the live data.
        // `{{spinner()}}` is clx's animated glyph, which keeps the background
        // render loop repainting (proof-of-life) even when the count is
        // momentarily static on a stalled filesystem. `progress_current` /
        // `progress_total` are still fed (below) for the OSC taskbar
        // indicator; no template bar consumes them.
        let phase0 = tty_phase_field("");
        let root = ProgressJobBuilder::new()
            .body(
                "{% if revealed %}{{aube}}  {{spinner()}} {{phase}}  \
                 {{count}}{{bytes}}{{rate}}{{eta}}{% endif %}",
            )
            .body_text(Some("{{aube}}{{phase}} {{count}}{{bytes}}{{rate}}{{eta}}"))
            .prop("aube", &header)
            .prop("phase", &phase0)
            .prop("count", "")
            .prop("bytes", "")
            .prop("rate", "")
            .prop("eta", "")
            .prop("revealed", &false)
            .progress_current(0)
            .progress_total(TTY_BAR_SCALE)
            // Hide on done: at teardown `finish()` (and the `Drop` safety net)
            // flip the root to `Done`, which then renders empty. That is what
            // makes the clear race-free — see `finish()` for why a plain
            // `stop_clear()` alone could leave a stray frame.
            .on_done(ProgressJobDoneBehavior::Hide)
            .start();
        let finished = Arc::new(AtomicBool::new(false));
        let phase_num = Arc::new(AtomicUsize::new(0));
        let files_linked = Arc::new(AtomicUsize::new(0));
        let revealed = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Condvar::new());
        let wake_lock = Arc::new(Mutex::new(()));
        let start = Instant::now();
        let ticker = Arc::new(Mutex::new(spawn_tty_ticker(TtyTicker {
            root: Arc::downgrade(&root),
            phase_num: phase_num.clone(),
            files_linked: files_linked.clone(),
            finished: finished.clone(),
            revealed: revealed.clone(),
            wake: wake.clone(),
            wake_lock: wake_lock.clone(),
            start,
        })));
        Self {
            mode: Mode::Tty {
                root,
                finished,
                total: Arc::new(AtomicUsize::new(0)),
                target_total: Arc::new(AtomicUsize::new(0)),
                reused: Arc::new(AtomicUsize::new(0)),
                downloaded: Arc::new(AtomicUsize::new(0)),
                phase_num,
                downloaded_bytes: Arc::new(AtomicU64::new(0)),
                estimated_bytes: Arc::new(AtomicU64::new(0)),
                fetch_start: Arc::new(OnceLock::new()),
                completed_at_fetch_start: Arc::new(AtomicUsize::new(usize::MAX)),
                start,
                revealed,
                wake,
                ticker,
            },
            unpacked_sizes: Arc::new(Mutex::new(HashMap::new())),
            files_linked,
        }
    }

    fn new_ci() -> Self {
        // Header + first progress line are deferred to the first heartbeat
        // tick (see `CiState::spawn_heartbeat`). A fast install that
        // finishes before the 2s heartbeat interval therefore prints
        // nothing at all — no header, no bar, no summary — which is what
        // we want for the no-op and near-no-op cases.
        let files_linked = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(CiState::new(files_linked.clone()));
        CiState::spawn_heartbeat(&state);
        Self {
            mode: Mode::Ci(state),
            unpacked_sizes: Arc::new(Mutex::new(HashMap::new())),
            files_linked,
        }
    }

    /// A clone of the shared live file-linking counter, handed to the
    /// linker (`Linker::with_link_progress`) so its materialize pass
    /// bumps it once per file. Both renderers read it to show the
    /// ticking `N files` count during linking.
    pub fn link_progress_counter(&self) -> Arc<AtomicUsize> {
        self.files_linked.clone()
    }

    /// Raise the resolving-phase denominator floor. Only ever
    /// increases the displayed total — a smaller `n` is silently
    /// ignored. Used by the install command to seed the resolving bar
    /// from any lockfile on disk and to surface the resolver's
    /// BFS-frontier high-water mark while resolution is in flight,
    /// so phase 1 renders a real bar instead of the empty-bar
    /// placeholder. No-op once resolution finishes — phase 2+ reads
    /// the actual count via `total` (set by [`set_total`]).
    pub fn set_total_floor(&self, n: usize) {
        match &self.mode {
            Mode::Tty { target_total, .. } => {
                target_total.fetch_max(n, Ordering::Relaxed);
                self.refresh_tty_bar();
            }
            Mode::Ci(s) => {
                s.target_total.fetch_max(n, Ordering::Relaxed);
            }
        }
    }

    /// Set the total (`resolved`) package count. Safe to call repeatedly.
    ///
    /// When this lowers the denominator (e.g. `filter_graph` just
    /// pruned platform-mismatched optionals after the streaming fetch
    /// already credited some of them), trim the `reused` numerator down
    /// so `reused + downloaded <= total`. Without this the final
    /// summary reports `reused N > resolved M` and the CI heartbeat
    /// trips `WARN_AUBE_PROGRESS_OVERFLOW` on a purely cosmetic
    /// inconsistency. Reused is the one trimmed (not downloaded)
    /// because registry tarballs are deferred at stream-time, so only
    /// the local-source / cached path can overshoot; downloaded
    /// reflects real network work and stays untouched.
    pub fn set_total(&self, total: usize) {
        match &self.mode {
            Mode::Tty {
                total: t,
                reused,
                downloaded,
                ..
            } => {
                t.store(total, Ordering::Relaxed);
                clamp_reused_to(reused, downloaded, total);
                // Refresh *after* clamping so the bar/count label
                // pick up the corrected numerator on the same tick
                // the denominator drops.
                self.refresh_tty_bar();
                self.refresh_eta();
            }
            Mode::Ci(s) => {
                s.resolved.store(total, Ordering::Relaxed);
                clamp_reused_to(&s.reused, &s.downloaded, total);
            }
        }
    }

    /// Atomically bump the total (`resolved`) by `n` packages.
    pub fn inc_total(&self, n: usize) {
        match &self.mode {
            Mode::Tty { total, .. } => {
                total.fetch_add(n, Ordering::Relaxed);
                self.refresh_tty_bar();
                self.refresh_eta();
            }
            Mode::Ci(s) => {
                s.resolved.fetch_add(n, Ordering::Relaxed);
            }
        }
    }

    /// Add `bytes` to the running estimated-total-download counter
    /// and record the per-`dep_path` contribution. Fed from
    /// `dist.unpackedSize` as resolver streams in packuments;
    /// surfaces as the `/ ~13.8 MB` suffix on the bytes segment so
    /// users have a sense of total install scope before the fetch
    /// finishes.
    ///
    /// The `dep_path` map lets [`reconcile_estimated_bytes`] later
    /// subtract platform-mismatched optionals that `filter_graph`
    /// drops, so the displayed estimate doesn't overstate the install
    /// size by the dropped-optionals' unpacked sizes. No-op when the
    /// packument lacks the field.
    pub fn inc_estimated_bytes(&self, dep_path: &str, bytes: u64) {
        // Streaming resolver should only see each dep_path once, but
        // a defensive duplicate stream would otherwise have the map
        // overwrite cleanly while the atomic running total
        // double-counts (the next `reconcile_estimated_bytes` would
        // re-sync from the map, but the bar would display an
        // inflated estimate in the meantime). Add only the *delta*
        // between the new value and any prior recorded value, so the
        // atomic stays in lockstep with the map.
        let prior = self
            .unpacked_sizes
            .lock()
            .unwrap()
            .insert(dep_path.to_string(), bytes)
            .unwrap_or(0);
        match &self.mode {
            Mode::Tty {
                estimated_bytes, ..
            } => {
                if prior > 0 {
                    estimated_bytes.fetch_sub(prior, Ordering::Relaxed);
                }
                estimated_bytes.fetch_add(bytes, Ordering::Relaxed);
                self.refresh_bytes_segment();
            }
            Mode::Ci(s) => {
                if prior > 0 {
                    s.estimated_bytes.fetch_sub(prior, Ordering::Relaxed);
                }
                s.estimated_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
        }
    }

    /// Recompute the estimated-total-download from the surviving set
    /// of dep_paths after `filter_graph` has pruned the resolver
    /// graph. Called from `install::run` once filtering completes —
    /// the running sum from `inc_estimated_bytes` includes platform-
    /// mismatched optionals that `filter_graph` just dropped, and
    /// without this reconcile the `~X MB` segment would overcount by
    /// their cumulative size. Mirrors the `set_total(graph.packages.len())`
    /// reconcile applied to the package denominator at the same site.
    pub fn reconcile_estimated_bytes<I, S>(&self, surviving_dep_paths: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let map = self.unpacked_sizes.lock().unwrap();
        let sum: u64 = surviving_dep_paths
            .into_iter()
            .filter_map(|k| map.get(k.as_ref()).copied())
            .sum();
        drop(map);
        match &self.mode {
            Mode::Tty {
                estimated_bytes, ..
            } => {
                estimated_bytes.store(sum, Ordering::Relaxed);
                self.refresh_bytes_segment();
            }
            Mode::Ci(s) => {
                s.estimated_bytes.store(sum, Ordering::Relaxed);
            }
        }
    }

    /// Set the phase label shown to the right of the header (e.g. "resolving",
    /// "fetching", "linking"). Empty string clears it.
    pub fn set_phase(&self, phase: &str) {
        match &self.mode {
            Mode::Tty {
                root,
                phase_num,
                fetch_start,
                reused,
                downloaded,
                completed_at_fetch_start,
                ..
            } => {
                // Fixed-width phase field (verb right-padded to the longest
                // possible verb), so the bar's `[` never shifts between phases.
                root.prop("phase", &tty_phase_field(phase));
                let n = match phase {
                    "resolving" => 1,
                    "fetching" => 2,
                    "linking" => 3,
                    _ => 0,
                };
                phase_num.store(n, Ordering::Relaxed);
                if n == 2 {
                    // Seed the rate denominator on the fetching transition.
                    // First-writer-wins; repeated calls are no-ops.
                    let _ = fetch_start.set(Instant::now());
                    // Capture the completion baseline so the ETA divides
                    // remaining work by *fetch-window* throughput, not by
                    // total install elapsed (which would inflate the
                    // estimate when resolve was slow). `compare_exchange`
                    // matches `fetch_start` first-writer-wins.
                    let baseline =
                        reused.load(Ordering::Relaxed) + downloaded.load(Ordering::Relaxed);
                    let _ = completed_at_fetch_start.compare_exchange(
                        usize::MAX,
                        baseline,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                } else if n == 3 {
                    // Linking phase: rate / ETA aren't meaningful — the
                    // network's done, the linker work is dominated by
                    // filesystem ops on a fixed package count. Clear
                    // both props so the "linking" word reads cleanly.
                    root.prop("rate", "");
                    root.prop("eta", "");
                }
                self.refresh_bytes_segment();
                self.refresh_rate();
                self.refresh_eta();
                // Phase change shifts the unified-progress slice
                // (resolving → fetching crosses the
                // `RESOLVE_BAR_WEIGHT` boundary; fetching → linking
                // locks at 100%), so the bar + count label must
                // recompute even when no counter advanced this turn.
                self.refresh_tty_bar();
            }
            Mode::Ci(s) => s.set_phase(phase),
        }
    }

    /// Credit `n` packages to the `reused` bucket: served from the global
    /// content-addressed store (cache hit) or materialized from a local
    /// `file:` / `link:` source — anything that didn't touch the network.
    pub fn inc_reused(&self, n: usize) {
        match &self.mode {
            Mode::Tty { reused, .. } => {
                reused.fetch_add(n, Ordering::Relaxed);
                self.refresh_tty_bar();
                self.refresh_eta();
            }
            Mode::Ci(s) => {
                s.reused.fetch_add(n, Ordering::Relaxed);
            }
        }
    }

    /// Credit `bytes` to the downloaded-bytes total. Called once per
    /// tarball after the registry fetch completes, on top of the per-package
    /// increment that `FetchRow::drop` contributes to the downloaded count.
    ///
    /// In TTY mode this refreshes the bytes / rate / ETA props on the
    /// animated bar. In CI mode the heartbeat re-renders from the
    /// cumulative byte counter on each tick; here we just bump that
    /// counter.
    pub fn inc_downloaded_bytes(&self, bytes: u64) {
        match &self.mode {
            Mode::Tty {
                downloaded_bytes, ..
            } => {
                downloaded_bytes.fetch_add(bytes, Ordering::Relaxed);
                self.refresh_bytes_segment();
                self.refresh_rate();
                // The package counter that drives ETA only changes via
                // `inc_reused` and `FetchRow::drop`, but bytes landing
                // is the strongest signal that fetch-window throughput
                // is still alive — refresh ETA on every byte event so
                // it keeps ticking down through long-lived downloads
                // even when no new package completion has fired.
                self.refresh_eta();
            }
            Mode::Ci(s) => {
                s.downloaded_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
        }
    }

    /// TTY-only: rebuild the `bytes` prop from the current downloaded /
    /// estimated counters. Picks shape based on what we know:
    ///   `4.2 MB / ~13.8 MB` when both are available, `4.2 MB` when
    /// TTY-only: recompute the bar fill + count label from the current
    /// TTY atomics and push them to clx. Shares the unified-progress
    /// math with CI mode via [`render::unified_progress`] and the
    /// count-segment shape via [`render::count_segment`], so a tweak
    /// to either lands in both renderers. clx's
    /// `progress_total`/`progress_current` are held at
    /// `TTY_BAR_SCALE / scaled-progress` to drive the flex-rendered
    /// bar; the user-facing cur/total label lives in the `count` prop
    /// so the scaled denominator never leaks into the text.
    fn refresh_tty_bar(&self) {
        let Mode::Tty {
            root,
            total,
            target_total,
            reused,
            downloaded,
            phase_num,
            start,
            revealed,
            ..
        } = &self.mode
        else {
            return;
        };
        // Reveal promptly off a resolve/fetch event once past the debounce,
        // so the line doesn't wait on the ticker's next poll.
        maybe_reveal(root, *start, revealed);
        refresh_tty_bar_from_atomics(
            root,
            total,
            target_total,
            reused,
            downloaded,
            phase_num,
            self.files_linked.load(Ordering::Relaxed),
        );
    }

    ///   only the running total is, `~13.8 MB` when only the estimate
    ///   is, empty otherwise. CI mode does this inside the heartbeat
    ///   render — no per-call refresh needed there.
    fn refresh_bytes_segment(&self) {
        let Mode::Tty {
            root,
            downloaded_bytes,
            estimated_bytes,
            total,
            downloaded,
            reused,
            phase_num,
            ..
        } = &self.mode
        else {
            return;
        };
        let bytes = downloaded_bytes.load(Ordering::Relaxed);
        // `estimated_bytes` is the raw `unpackedSize` sum; route it
        // through `estimated_total_download` to convert to the same
        // compressed-tarball units that `bytes` is in *and* blend in
        // the observed bytes-per-package average so the displayed
        // estimate converges to the real total as the install
        // progresses. CI mode does the same conversion inside its
        // render path.
        let estimated_unpacked = estimated_bytes.load(Ordering::Relaxed);
        // `total` here is the same atomic CI mode exposes as
        // `snap.resolved` — both grow as BFS resolution streams in
        // new packages. Use it directly as the "expected to download"
        // denominator so both render paths feed
        // `estimated_total_download` the same way and the displayed
        // `~XX MB` doesn't drift between modes mid-install. (It is
        // *not* `target_total`, which is the resolving-phase BFS
        // frontier hint used only for the resolve-slice bar fill.)
        let resolved_pkgs = total.load(Ordering::Relaxed);
        let downloaded_pkgs = downloaded.load(Ordering::Relaxed);
        let reused_pkgs = reused.load(Ordering::Relaxed);
        let expected_to_download = resolved_pkgs.saturating_sub(reused_pkgs);
        let estimated = render::estimated_total_download(
            estimated_unpacked,
            bytes,
            downloaded_pkgs,
            expected_to_download,
        );
        let phase = phase_num.load(Ordering::Relaxed);
        // The bytes segment is only useful during fetching. Hide it
        // before fetching (nothing downloaded yet) and during linking
        // (the post-install summary line reports the total, so showing
        // it inline is just duplicate noise).
        if phase != 2 || (bytes == 0 && estimated == 0) {
            root.prop("bytes", "");
            return;
        }
        let segment = if estimated > bytes && bytes > 0 {
            format!(
                " · {} {} {}",
                style::ebold(render::format_bytes(bytes)),
                style::edim("/"),
                style::edim(format!("~{}", render::format_bytes(estimated))),
            )
        } else if bytes > 0 {
            format!(" · {}", style::ebold(render::format_bytes(bytes)))
        } else {
            // bytes == 0, estimated > 0
            format!(
                " · {}",
                style::edim(format!("~{}", render::format_bytes(estimated)))
            )
        };
        root.prop("bytes", &segment);
    }

    /// TTY-only: rebuild the `rate` prop. Active during fetching only;
    /// cleared in resolving (no data) and linking (network done).
    fn refresh_rate(&self) {
        let Mode::Tty {
            root,
            phase_num,
            downloaded_bytes,
            fetch_start,
            ..
        } = &self.mode
        else {
            return;
        };
        if phase_num.load(Ordering::Relaxed) != 2 {
            root.prop("rate", "");
            return;
        }
        let bytes = downloaded_bytes.load(Ordering::Relaxed);
        let Some(start) = fetch_start.get() else {
            return;
        };
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if bytes == 0 || elapsed_ms == 0 {
            root.prop("rate", "");
            return;
        }
        let rate = bytes.saturating_mul(1000) / elapsed_ms;
        root.prop(
            "rate",
            &format!(
                " · {}",
                style::edim(format!("{}/s", render::format_bytes(rate)))
            ),
        );
    }

    /// TTY-only: rebuild the `eta` prop. `ETA …` while we don't have
    /// enough fetch-window data to extrapolate; `ETA Xs` once we do.
    /// Mirrors the CI render's eta_segment logic: divides remaining
    /// work by fetch-window throughput (`completed - baseline / fetch_elapsed_ms`)
    /// instead of total install elapsed, so a slow resolve doesn't
    /// inflate the early-fetching estimate.
    fn refresh_eta(&self) {
        let Mode::Tty {
            root,
            total,
            reused,
            downloaded,
            phase_num,
            fetch_start,
            completed_at_fetch_start,
            ..
        } = &self.mode
        else {
            return;
        };
        let phase = phase_num.load(Ordering::Relaxed);
        // Only show ETA in resolving + fetching. Linking is fast and
        // bounded — adding an ETA there would just flap around 0s.
        if phase == 0 || phase == 3 {
            root.prop("eta", "");
            return;
        }
        let total_n = total.load(Ordering::Relaxed);
        let completed =
            (reused.load(Ordering::Relaxed) + downloaded.load(Ordering::Relaxed)).min(total_n);
        let baseline = completed_at_fetch_start.load(Ordering::Relaxed);
        let placeholder = || root.prop("eta", &format!(" · {}", style::edim("ETA …")));
        if completed >= total_n || total_n == 0 || baseline == usize::MAX {
            placeholder();
            return;
        }
        let Some(start) = fetch_start.get() else {
            placeholder();
            return;
        };
        let fetch_elapsed_ms = start.elapsed().as_millis() as u64;
        let fetch_completed = completed.saturating_sub(baseline);
        if fetch_completed == 0 || fetch_elapsed_ms == 0 {
            placeholder();
            return;
        }
        let remaining = total_n - completed;
        let eta_ms = fetch_elapsed_ms.saturating_mul(remaining as u64) / fetch_completed as u64;
        let eta_str = format_duration(Duration::from_millis(eta_ms));
        root.prop(
            "eta",
            &format!(" · {}", style::edim(format!("ETA {eta_str}"))),
        );
    }

    /// Register an in-flight tarball fetch. Drop the returned `FetchRow` when
    /// the fetch completes to bump the `downloaded` bucket (which advances the
    /// bar's fetching slice). `name`/`version` are accepted for call-site
    /// symmetry but not displayed: the TTY renderer is single-line and shows no
    /// per-package rows, so the fetch only moves the unified bar. CI mode
    /// likewise just increments the `downloaded` counter on drop so the
    /// heartbeat advances.
    pub fn start_fetch(&self, name: &str, version: &str) -> FetchRow {
        let _ = (name, version);
        match &self.mode {
            Mode::Tty {
                root,
                total,
                target_total,
                reused,
                downloaded,
                phase_num,
                ..
            } => FetchRow {
                inner: FetchRowInner::Tty {
                    root: Arc::downgrade(root),
                    total: Arc::downgrade(total),
                    target_total: Arc::downgrade(target_total),
                    reused: Arc::downgrade(reused),
                    downloaded: Arc::downgrade(downloaded),
                    phase_num: Arc::downgrade(phase_num),
                },
                completed: false,
            },
            Mode::Ci(s) => FetchRow {
                inner: FetchRowInner::Ci(Arc::downgrade(s)),
                completed: false,
            },
        }
    }

    /// Finalize the progress display. TTY mode clears the single in-place
    /// progress line exactly once and stops the render loop, so the
    /// post-install dependency summary (or an early-return status line) takes
    /// its place with a clean handoff — no leftover bar frame, no flash, and no
    /// trailing blank line. CI mode blocks until the heartbeat thread has
    /// actually stopped so no stray tick can appear after this returns, and
    /// optionally writes the final framed `[ ✓ … ]` status line.
    /// Idempotent.
    ///
    /// `print_ci_summary`: set to `false` when a later call site will
    /// print its own end-of-install line (so the main install path
    /// doesn't double up with [`print_install_summary`]). Set to `true`
    /// for early-return paths (`--lockfile-only`, drift check) that
    /// want the framed summary to remain the end of CI log output. Ignored in
    /// TTY mode, which prints its summary separately via
    /// [`print_install_summary`] after this clears the bar.
    pub fn finish(&self, print_ci_summary: bool) {
        match &self.mode {
            Mode::Tty {
                root,
                finished,
                wake,
                ticker,
                ..
            } => {
                // Stop the link-progress ticker FIRST (latch `finished`, wake
                // it, join) so no late `count` write can land after the clear
                // below and repaint a stray line. `stop_tty_ticker` also sets
                // `finished`, which tells `Drop` the teardown already happened.
                stop_tty_ticker(finished, wake, ticker);
                // Clear the line instead of leaving a final frame: the success
                // line that follows (`✓ installed N packages`) is the
                // completion cue, so a lingering spinner line above it would
                // just be a redundant second `nub …` line bracketing the
                // dependency summary. Because the renderer is a single line,
                // the clear is a one-row erase — no multi-line region collapse,
                // no flash.
                //
                // Flip the root to `Done` (it is `on_done = Hide`, so it now
                // renders empty) BEFORE stopping the loop. This is what makes
                // the teardown race-free: `set_status(Done)` runs a synchronous
                // `refresh_once()` under clx's `REFRESH_LOCK`, which serializes
                // against the background render thread, so any in-flight frame
                // is followed by an empty render that erases it. A bare
                // `stop_clear()` takes only `TERM_LOCK` (not `REFRESH_LOCK`), so
                // a render-thread frame could land *after* the clear and leave
                // a stray line; rendering the root empty closes that window —
                // even a late frame paints nothing. `stop_clear` is itself
                // idempotent (once `STOPPING` latches and `LINES` is 0 a repeat
                // erases nothing), covering a double `finish()`.
                root.set_status(ProgressStatus::Done);
                clx::progress::stop_clear();
            }
            Mode::Ci(s) => s.stop(print_ci_summary),
        }
    }

    /// Emit the post-install summary line after the progress display has
    /// been torn down. Two shapes:
    ///
    /// * `linked > 0` — `aube VERSION by jdx.dev · ✓ installed N packages
    ///   in Xs`, TTY-only (CI mode prints its own framed `✓` summary
    ///   from the heartbeat's final tick).
    /// * `linked == 0 && top_level_linked == 0` — `Already up to date`
    ///   (matches pnpm), printed in both TTY and CI modes so cache-only
    ///   runs confirm nothing needed doing. Stays silent in reporter
    ///   modes where `prog_ref` is `None`.
    ///
    /// The `top_level_linked` guard distinguishes a true no-op from the
    /// `rm -rf node_modules && aube install` case where the global store
    /// was warm (so `packages_linked` is 0) but every top-level symlink
    /// had to be recreated — that's not "up to date" from the user's
    /// perspective.
    ///
    /// **Safety:** must be called *after* [`InstallProgress::finish`]. The
    /// write goes straight to stderr without routing through
    /// `PausingWriter` or `with_terminal_lock`, which is only safe once
    /// `finish()` has synchronously stopped the render loop. A new call site
    /// placed before `finish()` would silently race the animated display.
    pub fn print_install_summary(
        &self,
        linked: usize,
        top_level_linked: usize,
        total_packages: usize,
        elapsed: Duration,
    ) {
        if linked == 0 && top_level_linked == 0 {
            let body = if total_packages == 0 {
                "Already up to date".to_string()
            } else {
                format!(
                    "Already up to date ({})",
                    pluralizer::pluralize("package", total_packages as isize, true)
                )
            };
            // Only the check mark is green so it stays the visual
            // success cue without the whole message bleeding green.
            // Same single-line `aube VERSION by jdx.dev · ✓ msg` shape
            // for both TTY and CI modes; CI mode's heartbeat may have
            // emitted intermediate progress lines above this.
            let msg = format!("{} {}", style::egreen("✓").bold(), style::ebold(&body));
            let line = aube_prefix_line(&msg);
            let _ = writeln!(std::io::stderr(), "{line}");
            return;
        }
        if linked == 0 {
            return;
        }
        // CI mode prints its own multi-segment summary from the
        // heartbeat's final tick (resolve / reused / downloaded
        // breakdown). For fast installs that never hit the heartbeat,
        // print the single-line summary here so the user still sees
        // a confirmation. TTY mode always prints here.
        let needs_summary = match &self.mode {
            Mode::Tty { .. } => true,
            Mode::Ci(s) => !s.shown.load(Ordering::Relaxed),
        };
        if !needs_summary {
            return;
        }
        // Only the check mark is green so the success cue is sharp
        // without the whole sentence bleeding into one color block.
        let msg = format!(
            "{} installed {} in {}",
            style::egreen("✓").bold(),
            style::ebold(pluralizer::pluralize("package", linked as isize, true)),
            style::edim(format_duration(elapsed)),
        );
        let line = aube_prefix_line(&msg);
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

/// The fixed-width phase field — the `<verb>` right-padded to
/// [`TTY_PHASE_W`], or an all-blank field of the SAME width when no phase is
/// active. Padding to the widest verb is what pins the `count` column (and
/// everything after it) to one place regardless of which verb is showing —
/// only the count/spinner move as the phase changes, and they're the live
/// data. The verb takes a cyan/dim accent so it reads as a status label. The
/// spinner and the separating spaces live in the body template, not here, so
/// the field itself is exactly [`TTY_PHASE_W`] display columns.
fn tty_phase_field(phase: &str) -> String {
    if phase.is_empty() {
        return " ".repeat(TTY_PHASE_W);
    }
    let colored = match phase {
        "resolving" | "linking" => style::ecyan(phase).to_string(),
        _ => style::edim(phase).to_string(),
    };
    // Pad on the PLAIN verb width (ANSI codes carry zero display width), so the
    // trailing spaces land outside the color span and the field's visible width
    // is exactly `TTY_PHASE_W`.
    let pad = TTY_PHASE_W.saturating_sub(phase.len());
    format!("{}{}", colored, " ".repeat(pad))
}

/// The counter field. Linking shows a live, monotonically growing file
/// count — `12043 files` — with NO denominator (the total isn't known
/// until the pass ends); this is the number the user watches tick up on
/// a slow filesystem, and since bytes/rate/ETA are all cleared during
/// linking its digit growth shifts nothing to its right. Resolving and
/// fetching keep the fixed-column `577/623 pkgs` shape (numerator
/// right-aligned, total left-aligned to [`TTY_COUNT_DIGITS`]) so the
/// trailing byte/rate/ETA segment holds a constant column for installs
/// up to 9999 packages. Phase 0 renders empty — nothing trails it.
/// TTY-specific (not the shared `render::count_segment`, which the
/// append-only CI renderer uses).
fn tty_count_field(snap: ci::Snap, completed: usize) -> String {
    if snap.phase == 3 {
        return format!(
            "{} {}",
            style::ebold(snap.files_linked),
            style::edim("files"),
        );
    }
    let (cur, total) = match snap.phase {
        0 => return String::new(),
        1 if snap.target_total > snap.resolved => (snap.resolved, Some(snap.target_total)),
        1 => (snap.resolved, None),
        _ => (completed, Some(snap.resolved)),
    };
    let cur_s = style::ebold(format!("{cur:>w$}", w = TTY_COUNT_DIGITS)).to_string();
    let mid = match total {
        Some(t) => format!(
            "{}{}",
            style::edim("/"),
            style::ebold(format!("{t:<w$}", w = TTY_COUNT_DIGITS)),
        ),
        None => " ".repeat(TTY_COUNT_DIGITS + 1),
    };
    format!("{cur_s}{mid} {}", style::edim("pkgs"))
}

/// TTY-only count refresh primitive. Strongly-typed `&AtomicUsize` /
/// `&ProgressJob` references so both the `InstallProgress`
/// method (which holds Arcs) and `FetchRow::drop` (which holds
/// Weaks and upgrades them) can share the math without duplicating
/// the snapshot/scale/prop-set sequence. Sets the user-facing `count`
/// prop via the fixed-column [`tty_count_field`] and feeds the OSC
/// taskbar indicator via `progress_current`. `files` is the live
/// file-linking count (0 outside the linking phase); resolve/fetch
/// callers pass it as 0 since only phase 3 consults it.
fn refresh_tty_bar_from_atomics(
    root: &Arc<ProgressJob>,
    total: &AtomicUsize,
    target_total: &AtomicUsize,
    reused: &AtomicUsize,
    downloaded: &AtomicUsize,
    phase_num: &AtomicUsize,
    files: usize,
) {
    let phase = phase_num.load(Ordering::Relaxed);
    let resolved = total.load(Ordering::Relaxed);
    let target = target_total.load(Ordering::Relaxed);
    let r = reused.load(Ordering::Relaxed);
    let d = downloaded.load(Ordering::Relaxed);
    // Reuse the CI-mode `Snap` shape so the shared helpers don't
    // need a TTY-specific variant. The byte/rate/ETA fields aren't
    // consulted by `unified_progress` or `tty_count_field`; their
    // zero values are inert.
    let snap = ci::Snap {
        phase,
        resolved,
        target_total: target,
        reused: r,
        downloaded: d,
        bytes: 0,
        estimated: 0,
        files_linked: files,
        fetch_elapsed_ms: 0,
        completed_at_fetch_start: None,
    };
    // Same clamp the CI render applies — keeps the numerator from
    // exceeding the resolved denominator if a deferred-package
    // catch-up reorders against `set_total`.
    let completed = (r + d).min(resolved);
    let progress = render::unified_progress(snap, completed);
    // Feed clx's OSC terminal-progress indicator (the taskbar %). No
    // visible template bar consumes this — the line is spinner + count.
    let scaled = ((progress * TTY_BAR_SCALE as f64).round() as usize).min(TTY_BAR_SCALE);
    root.progress_current(scaled);
    root.prop("count", &tty_count_field(snap, completed));
}

/// Reveal the animated line once the debounce window has elapsed.
/// Flips the `revealed` prop (and thus the body's `{% if revealed %}`
/// guard) exactly once — the `swap` latch means repeated calls after
/// the first are no-ops. Before this fires the body renders empty and
/// clx drops the row, so a sub-debounce install shows nothing.
fn maybe_reveal(root: &Arc<ProgressJob>, start: Instant, revealed: &AtomicBool) {
    if start.elapsed() >= TTY_REVEAL_DELAY && !revealed.swap(true, Ordering::Relaxed) {
        root.prop("revealed", &true);
    }
}

/// Fields the link-progress ticker thread needs. Bundled so the spawn
/// call site reads cleanly. All handles are weak/shared so the ticker
/// never pins the display alive (mirrors `FetchRow`'s weak-ref
/// discipline): it holds a `Weak<ProgressJob>` and exits when the root
/// is gone or `finished` latches.
struct TtyTicker {
    root: Weak<ProgressJob>,
    phase_num: Arc<AtomicUsize>,
    files_linked: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
    revealed: Arc<AtomicBool>,
    wake: Arc<Condvar>,
    wake_lock: Arc<Mutex<()>>,
    start: Instant,
}

/// Spawn the link-progress ticker: a background thread that drives the
/// two things the event-driven refreshers can't during linking — the
/// debounce reveal (guaranteed within one tick of crossing the delay,
/// even if no counter event fires) and the live file count (the linker
/// bumps an atomic with no clx event, so the ticker re-reads it and
/// repaints the `count` prop). It exits promptly on `finish()` via the
/// condvar, or on its own if the root job is dropped.
///
/// Cosmetic, never load-bearing: on `Builder::spawn` failure (thread/PID
/// exhaustion) the install proceeds with no live tick — the final
/// summary still prints. Mirrors the CI heartbeat's spawn discipline.
fn spawn_tty_ticker(t: TtyTicker) -> Option<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("aube-tty-progress".into())
        .spawn(move || {
            loop {
                if t.finished.load(Ordering::Relaxed) {
                    break;
                }
                let Some(root) = t.root.upgrade() else {
                    break;
                };
                maybe_reveal(&root, t.start, &t.revealed);
                if t.phase_num.load(Ordering::Relaxed) == 3 {
                    let files = t.files_linked.load(Ordering::Relaxed);
                    root.prop("count", &tty_count_field(linking_snap(files), 0));
                }
                drop(root);
                let guard = t.wake_lock.lock().unwrap();
                let _ = t.wake.wait_timeout(guard, TTY_TICK_INTERVAL).unwrap();
            }
        })
        .ok()
}

/// A minimal linking-phase `Snap` carrying just the live file count —
/// all the ticker needs to render the `N files` segment via the shared
/// [`tty_count_field`].
fn linking_snap(files: usize) -> ci::Snap {
    ci::Snap {
        phase: 3,
        resolved: 0,
        target_total: 0,
        reused: 0,
        downloaded: 0,
        bytes: 0,
        estimated: 0,
        files_linked: files,
        fetch_elapsed_ms: 0,
        completed_at_fetch_start: None,
    }
}

/// Stop the link-progress ticker: latch `finished`, wake it off its
/// poll wait, and join so no late `count` write can repaint after the
/// display is cleared. Idempotent — a second call finds the handle
/// already taken. Shared by `finish()` and the `Drop` safety net.
fn stop_tty_ticker(
    finished: &AtomicBool,
    wake: &Condvar,
    ticker: &Mutex<Option<thread::JoinHandle<()>>>,
) {
    finished.store(true, Ordering::Relaxed);
    wake.notify_all();
    if let Some(handle) = ticker.lock().unwrap().take() {
        let _ = handle.join();
    }
}

impl Drop for InstallProgress {
    /// Safety net: if `install::run` bails through `?` without reaching
    /// `finish()` (flaky network, lockfile parse error, linker failure, …)
    /// the renderer would otherwise be left running. We only tear down
    /// when *this* instance is the last live clone, not when an earlier
    /// clone (e.g. the one handed to the fresh-resolve fetch coordinator)
    /// drops while the install is still in flight.
    ///
    /// CI mode can't use `Arc::strong_count` for this check because the
    /// heartbeat thread holds its own clone of `Arc<CiState>` for the
    /// entire run. Instead, it tracks the live-clone count in a separate
    /// `CiState::alive` atomic, incremented in `Clone` and decremented
    /// here. Error paths drop without printing the `Done in Xs` summary
    /// — the heartbeat still gets joined so no stray tick escapes.
    fn drop(&mut self) {
        match &self.mode {
            Mode::Tty {
                root,
                finished,
                wake,
                ticker,
                ..
            } => {
                if Arc::strong_count(root) == 1 && !finished.load(Ordering::Relaxed) {
                    // Last live clone bailed without `finish()` (error path):
                    // stop the ticker before clearing so a late `count` write
                    // can't repaint. The ticker holds a `Weak<root>`, so it
                    // never affected the `strong_count == 1` check above.
                    stop_tty_ticker(finished, wake, ticker);
                    root.set_status(ProgressStatus::Done);
                    clx::progress::stop_clear();
                }
            }
            Mode::Ci(s) => {
                if s.alive.fetch_sub(1, Ordering::Relaxed) == 1 {
                    s.stop(false);
                }
            }
        }
    }
}

/// A single in-flight fetch. Dropping completes it by bumping the download
/// counter (which advances the bar's fetching slice in TTY mode and the
/// heartbeat in CI mode). No per-package row is shown in either mode.
pub struct FetchRow {
    inner: FetchRowInner,
    completed: bool,
}

enum FetchRowInner {
    Tty {
        /// Weak refs to every TTY counter the unified-bar refresh reads.
        /// Bundled here so `FetchRow::drop` can recompute the bar fill + count
        /// label after bumping `downloaded`, without a back-pointer to
        /// `InstallProgress` (which is not itself reference-counted). Mirrors
        /// the field set `refresh_tty_bar` reads off `Mode::Tty`. Weak so an
        /// orphaned row (a fetch task still in flight after an error
        /// short-circuits the install) can't pin the root job alive and block
        /// `InstallProgress::Drop` from clearing the display.
        root: Weak<ProgressJob>,
        total: Weak<AtomicUsize>,
        target_total: Weak<AtomicUsize>,
        reused: Weak<AtomicUsize>,
        downloaded: Weak<AtomicUsize>,
        phase_num: Weak<AtomicUsize>,
    },
    /// Matches the TTY variant's weak-ref discipline: orphaned CI fetch
    /// rows shouldn't prevent `CiState` from being dropped after the
    /// last `InstallProgress` clone is gone.
    Ci(Weak<CiState>),
}

impl FetchRow {
    fn finish_inner(&mut self) {
        if self.completed {
            return;
        }
        self.completed = true;
        match &self.inner {
            FetchRowInner::Tty {
                root,
                total,
                target_total,
                reused,
                downloaded,
                phase_num,
            } => {
                // Bump the downloaded counter, then refresh the unified bar
                // (clx `progress_current` + `count` prop) by upgrading the weak
                // refs to each TTY atomic. `refresh_eta` is *not* called here —
                // the ETA prop is recomputed on every `inc_downloaded_bytes`
                // event, which fires once per tarball before this drop. The
                // off-by-one (ETA computed against pre-bump `downloaded`)
                // self-corrects on the next fetch's bytes; for the very last
                // fetch, `set_phase("linking")` immediately clears the prop.
                if let Some(d) = downloaded.upgrade() {
                    d.fetch_add(1, Ordering::Relaxed);
                }
                if let (
                    Some(root),
                    Some(total),
                    Some(target_total),
                    Some(reused),
                    Some(downloaded),
                    Some(phase_num),
                ) = (
                    root.upgrade(),
                    total.upgrade(),
                    target_total.upgrade(),
                    reused.upgrade(),
                    downloaded.upgrade(),
                    phase_num.upgrade(),
                ) {
                    // A fetch completion is always phase 2 (fetching), where
                    // the live file count is irrelevant — pass 0.
                    refresh_tty_bar_from_atomics(
                        &root,
                        &total,
                        &target_total,
                        &reused,
                        &downloaded,
                        &phase_num,
                        0,
                    );
                }
            }
            FetchRowInner::Ci(weak) => {
                if let Some(s) = weak.upgrade() {
                    s.downloaded.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

impl Drop for FetchRow {
    fn drop(&mut self) {
        self.finish_inner();
    }
}

/// A `tracing_subscriber` writer that coordinates with clx so log
/// events don't get overwritten by the animated progress display.
///
/// Default `std::io::stderr` writes race the render loop: a `warn!`
/// emitted mid-frame lands in the middle of a redraw, leaving the bar
/// fragments smeared across the log line (and the log line smeared
/// across the bar) until the next tick repaints over it.
///
/// `PausingWriter` fixes this by buffering each event in-memory and
/// flushing the whole buffer atomically at the end of the event:
///
///   1. `make_writer` returns a fresh buffered guard — one per event.
///   2. The fmt layer writes the formatted record (level prefix,
///      message, fields, trailing newline) into the guard's buffer.
///   3. On drop, the guard takes clx's terminal lock, pauses the
///      render loop, writes the whole buffer in a single `write_all`,
///      then resumes.
///
/// Holding the terminal lock across the pause/write/resume window
/// serializes against `ProgressJob::println` and the render thread,
/// so neither can interleave half a frame mid-event. In text mode
/// (`-v`, `--silent`, append-only, ndjson) the progress display
/// isn't running; pause/resume become benign no-ops and the event
/// still flushes cleanly.
/// Print a message to stderr safely while the install progress bar
/// may be active. Direct `eprintln!` during an active bar smears
/// output across frames (bar paints over half the message, next tick
/// repaints over what remains). Use this for warnings that need to
/// surface mid-install like peer-dep errors, allowBuilds policy
/// warnings, retry notifications, etc. If no bar is up, degenerates
/// to a plain stderr write. Trailing newline is appended. Call sites
/// that already hold a bar handle can use ProgressJob::println
/// instead, but this works without one.
pub fn safe_eprintln(msg: &str) {
    use std::io::Write;
    let was_paused = clx::progress::is_paused();
    if !was_paused {
        clx::progress::pause();
    }
    let _: () = clx::progress::with_terminal_lock(|| {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{msg}");
        let _ = stderr.flush();
    });
    if !was_paused {
        clx::progress::resume();
    }
}

#[derive(Clone, Copy, Default)]
pub struct PausingWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for PausingWriter {
    type Writer = PausingWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        PausingWriterGuard { buf: Vec::new() }
    }
}

/// Per-event writer guard returned by [`PausingWriter::make_writer`].
/// Accumulates into `buf` and flushes once on drop. See `PausingWriter`
/// for the full pause/write/resume protocol.
pub struct PausingWriterGuard {
    buf: Vec<u8>,
}

impl Write for PausingWriterGuard {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for PausingWriterGuard {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let buf = std::mem::take(&mut self.buf);
        // Pause *before* taking `TERM_LOCK`: `pause()` internally
        // calls `clear()`, which also grabs `TERM_LOCK`, and
        // `std::sync::Mutex` isn't reentrant — taking the lock first
        // would deadlock. Same ordering `ProgressJob::println` uses.
        //
        // The `is_paused()` → `pause()` check is intentionally not
        // atomic. Two guards dropping concurrently can both observe
        // `was_paused = false`, and the first `resume()` can restart
        // the render loop before the second thread's write lands.
        // That's a benign visual artifact (the progress bar may
        // briefly redraw between the two log lines), not a correctness
        // hazard: byte-level atomicity comes from `with_terminal_lock`
        // below, which serializes every writer — render thread,
        // `ProgressJob::println`, and other `PausingWriterGuard`
        // drops. `pause`/`resume` are best-effort visual guards on
        // top of that hard serialization.
        let was_paused = clx::progress::is_paused();
        if !was_paused {
            clx::progress::pause();
        }
        // Hold `TERM_LOCK` across the actual write so the render
        // thread (which also takes it before `write_frame`) and any
        // concurrent `ProgressJob::println` can't interleave between
        // our bytes. `with_terminal_lock` returns `()` here; the
        // explicit annotation silences its `#[must_use]`.
        let _: () = clx::progress::with_terminal_lock(|| {
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(&buf);
            let _ = stderr.flush();
        });
        if !was_paused {
            clx::progress::resume();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display width of a styled string: strip SGR escapes, then count chars
    /// (every glyph the bar uses — ASCII, `—`, `█`, `░`, `·`, `/` — is one
    /// display column).
    fn vis_width(s: &str) -> usize {
        let mut out = 0usize;
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for esc in chars.by_ref() {
                    if esc.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out += 1;
        }
        out
    }

    fn snap_at(phase: usize, resolved: usize, target_total: usize, completed: usize) -> ci::Snap {
        ci::Snap {
            phase,
            resolved,
            target_total,
            reused: completed,
            downloaded: 0,
            bytes: 0,
            estimated: 0,
            files_linked: 0,
            fetch_elapsed_ms: 0,
            completed_at_fetch_start: None,
        }
    }

    /// The maintainer's no-layout-shift requirement: the phase verb is
    /// right-padded to a fixed width so the `count` column — and everything
    /// after it — holds byte-stable when the phase word changes. That reduces
    /// to: `tty_phase_field` has the same display width for every verb (and
    /// when empty), namely `TTY_PHASE_W`. The spinner + separating spaces live
    /// in the body template, so they don't enter this measurement.
    #[test]
    fn phase_field_is_constant_width_across_verbs() {
        let widths: Vec<usize> = TTY_PHASE_VERBS
            .iter()
            .copied()
            .chain(["", "downloading-future-verb-not-in-set"]) // empty + an over-long verb
            .map(|v| vis_width(&tty_phase_field(v)))
            .collect();
        // Every KNOWN verb + the empty field share one width — the longest
        // known verb sets the pad. An over-long verb (not in the set) is the
        // one case that may exceed it, which is fine: the set is exhaustive.
        let expected = TTY_PHASE_W;
        for (v, w) in TTY_PHASE_VERBS.iter().chain(&[""]).zip(widths.iter()) {
            assert_eq!(*w, expected, "phase field width drifted for {v:?}");
        }
    }

    /// Resolving/fetching hold one column across digit counts (up to 9999),
    /// numerator right-aligned — `7/331` and `577/623` line up — so the
    /// trailing byte/rate/ETA segment never shifts.
    #[test]
    fn count_field_is_constant_width_up_to_four_digits() {
        let expected = 2 * TTY_COUNT_DIGITS + 6; // cur(D) + "/" + total(D) + " pkgs"
        let cases = [
            snap_at(1, 7, 331, 0),     // resolving with estimate: 7/331
            snap_at(1, 84, 84, 0),     // resolving, no estimate yet (target == resolved): bare 84
            snap_at(2, 623, 623, 7),   // fetching: 7/623
            snap_at(2, 623, 623, 577), // fetching: 577/623
            snap_at(4, 9999, 9999, 1), // done summary: huge but still 4-digit
        ];
        for s in cases {
            let completed = s.reused;
            let w = vis_width(&tty_count_field(s, completed));
            assert_eq!(
                w, expected,
                "count width drifted at phase {} cur {} total {}",
                s.phase, completed, s.resolved
            );
        }
        // Phase 0 renders nothing (no trailing field to hold).
        assert_eq!(tty_count_field(snap_at(0, 0, 0, 0), 0), "");
        // Numerator is right-aligned: a 1-digit count is space-padded so its
        // right edge lines up with a 3-digit count.
        let one = tty_count_field(snap_at(2, 331, 331, 7), 7);
        assert!(vis_width(&one) == expected, "right-align padding lost");
    }

    /// Linking shows the live, growing file count with no denominator — the
    /// number the user watches tick up on a slow link. It reads from
    /// `files_linked`, not the package counters, and carries the ` files`
    /// unit (never ` pkgs`).
    #[test]
    fn linking_count_shows_live_file_count() {
        let mut s = snap_at(3, 800, 800, 800);
        s.files_linked = 12_043;
        let field = tty_count_field(s, 800);
        let plain = strip_ansi_local(&field);
        assert_eq!(plain, "12043 files", "got: {plain}");
    }

    fn strip_ansi_local(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for esc in chars.by_ref() {
                    if esc.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn clamp_reused_trims_overshoot_after_downward_rebase() {
        // Streamed-then-pruned scenario: resolver bumped reused for
        // local sources that filter_graph later GC'd as unreachable
        // through dropped optional edges. set_total(graph.packages.len())
        // then has to trim the numerator so it doesn't exceed the new
        // denominator.
        let reused = AtomicUsize::new(229);
        let downloaded = AtomicUsize::new(0);
        clamp_reused_to(&reused, &downloaded, 226);
        assert_eq!(reused.load(Ordering::Relaxed), 226);
        assert_eq!(downloaded.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn clamp_reused_preserves_downloaded() {
        // Trim reused (cosmetic over-credit from streaming) but never
        // touch downloaded — that count reflects real network work and
        // registry tarballs are deferred at stream-time, so it can't
        // overshoot on its own.
        let reused = AtomicUsize::new(50);
        let downloaded = AtomicUsize::new(80);
        clamp_reused_to(&reused, &downloaded, 100);
        assert_eq!(reused.load(Ordering::Relaxed), 20);
        assert_eq!(downloaded.load(Ordering::Relaxed), 80);
    }

    #[test]
    fn clamp_reused_is_noop_when_within_cap() {
        let reused = AtomicUsize::new(40);
        let downloaded = AtomicUsize::new(30);
        clamp_reused_to(&reused, &downloaded, 100);
        assert_eq!(reused.load(Ordering::Relaxed), 40);
        assert_eq!(downloaded.load(Ordering::Relaxed), 30);
    }

    #[test]
    fn clamp_reused_floors_at_zero_when_downloaded_exceeds_total() {
        // Defensive: if downloaded somehow exceeds total (shouldn't
        // happen in practice — deferral prevents it), still cap reused
        // at zero rather than wrapping.
        let reused = AtomicUsize::new(5);
        let downloaded = AtomicUsize::new(110);
        clamp_reused_to(&reused, &downloaded, 100);
        assert_eq!(reused.load(Ordering::Relaxed), 0);
        assert_eq!(downloaded.load(Ordering::Relaxed), 110);
    }
}
