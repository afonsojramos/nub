//! Node process spawning with augmentation: flag injection, PATH shim,
//! preload injection, env loading. The central pipeline that composes
//! all of Nub's runtime augmentation into a single child-process spawn.

use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

use super::discovery::ResolvedNode;
use super::flags;

/// Terminating-signal forwarding to the current child, registered once per
/// process. Nub catches SIGINT (Ctrl-C), SIGTERM (docker stop / systemd / CI
/// cancel) and SIGHUP (terminal hangup) and re-sends the SAME signal to the Node
/// child, so the child runs its own handler and exits with the matching code —
/// instead of being reparented to PID 1 and running forever, which is what
/// happened when only SIGINT was handled. A single background thread reads the
/// current child's pid from a global atomic that each spawn updates, so
/// sequential / re-entrant spawns forward to the right child, and a stray signal
/// after a child exits (pid cleared to 0) is a no-op rather than a kill of a
/// reused pid.
///
/// The diagnostic signals SIGUSR1, SIGUSR2 and SIGQUIT are forwarded too, for a
/// different reason than the terminating ones. Node assigns them meaning at the
/// child: SIGUSR1 activates the inspector / debugger, SIGUSR2 is the conventional
/// `--report-signal` trigger (and what tools like nodemon send), and SIGQUIT
/// reaches V8. Their DEFAULT disposition would terminate (SIGUSR1/USR2) or
/// terminate-and-core (SIGQUIT) the resident Rust PARENT — killing nub before the
/// child ever sees them. Registering a `signal-hook` handler for each overrides
/// that default disposition (the parent no longer dies), and the forwarder relays
/// the same signo to the child. Crucially, nub does NOT exit on these: unlike the
/// terminating set, nub keeps running and waits for the child after relaying, so
/// e.g. `kill -USR2 <nub>` writes a diagnostic report in the child and both
/// processes stay alive — exactly as if `node` had received the signal directly.
#[cfg(unix)]
mod ctrl_c {
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, Ordering};

    // The forward TARGET, as the argument to `kill(2)`: a POSITIVE pid signals one
    // process (the file-run path's `node`, which IS the leaf); a NEGATIVE value
    // signals the whole PROCESS GROUP `-value` (the script path's `sh -c` child,
    // made a group leader via `setpgid`, so the signal reaches `sh` AND the `node`
    // it forks — a non-interactive `sh -c` does NOT relay signals to a forked
    // child, so single-pid delivery left the workload orphaned under dash). 0 = no
    // child tracked.
    static CURRENT_TARGET: AtomicI32 = AtomicI32::new(0);
    static REGISTERED: Once = Once::new();

    /// Record the `kill(2)` target (see [`CURRENT_TARGET`]), registering the signal
    /// handler on the first call. Later calls just update the target.
    pub(super) fn track(target: i32) {
        CURRENT_TARGET.store(target, Ordering::SeqCst);
        REGISTERED.call_once(|| {
            use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
            use signal_hook::iterator::Signals;
            // signal-hook delivers the signo on a normal thread (via a self-pipe),
            // so `kill` here is not in an async-signal context. Forward the EXACT
            // signal Nub received to the child — TERM→TERM, HUP→HUP, INT→INT, and
            // the diagnostic USR1/USR2/QUIT→same — so the child runs its own handler
            // (terminating set exits 128+signo, byte-for-byte with plain Node; the
            // diagnostic set does whatever Node does with it). Merely listing a signal
            // in `Signals::new` installs a signal-hook handler for it, which overrides
            // the kernel's default disposition: that is what stops USR1/USR2 (default:
            // terminate) and QUIT (default: terminate+core) from killing the resident
            // parent before they can be relayed. If registration fails we simply don't
            // forward (the pre-existing no-handler behavior), never crash.
            if let Ok(mut signals) =
                Signals::new([SIGINT, SIGTERM, SIGHUP, SIGUSR1, SIGUSR2, SIGQUIT])
            {
                std::thread::spawn(move || {
                    for signo in signals.forever() {
                        let target = CURRENT_TARGET.load(Ordering::SeqCst);
                        if target != 0 {
                            // SAFETY: kill(2) with a stored-live target + the received
                            // signal. A positive target signals one process; a negative
                            // one signals process group `-target`. Benign if the
                            // child/group already exited (ESRCH); cleared to 0 on exit.
                            unsafe {
                                libc::kill(target, signo);
                            }
                        }
                    }
                });
            }
        });
    }

    /// Clear the current target after the child exits.
    pub(super) fn untrack() {
        CURRENT_TARGET.store(0, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn current() -> i32 {
        CURRENT_TARGET.load(Ordering::SeqCst)
    }
}

/// Track a child's process GROUP as the signal-forward target — for the `nub run`
/// script path, whose child is `sh -c <script>`. The script child is made a group
/// leader by [`group_on_spawn`], so signaling group `-pid` reaches `sh` AND the
/// `node` it forks. This is what `spawn_node`'s single-pid tracking can't do for
/// scripts: a non-interactive `sh -c` does not relay a forwarded signal to a
/// forked child, so `docker stop` on a `nub run` entrypoint orphaned the workload
/// (the Nub leader and `sh` exited; the `node` subtree ran on). No-op off Unix.
pub fn track_child_group(pid: u32) {
    #[cfg(unix)]
    ctrl_c::track(-(pid as i32));
    #[cfg(not(unix))]
    let _ = pid;
}

/// Clear the tracked child/group after it exits — pair with [`track_child_group`].
pub fn untrack_child() {
    #[cfg(unix)]
    ctrl_c::untrack();
}

/// Put the spawned child in its own process group (`setpgid(0, 0)` at exec) so
/// [`track_child_group`] can signal the whole subtree. No-op off Unix.
pub fn group_on_spawn(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid(0, 0) only repoints the child's own process-group id
        // between fork and exec — async-signal-safe, touches no parent state.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Spawn `cmd` in its own process group and wait, forwarding terminating signals
/// to the whole group while it runs — the signal-faithful, subtree-reaching
/// equivalent of `cmd.status()` for a `sh -c <script>` child. Use for the `nub
/// run` script path so `docker stop` / Ctrl-C reach the script and everything it
/// spawns, not just Nub's leader.
pub fn status_forwarding_signals(cmd: &mut Command) -> std::io::Result<ExitStatus> {
    group_on_spawn(cmd);
    let mut child = cmd.spawn()?;
    track_child_group(child.id());
    let status = child.wait();
    untrack_child();
    status
}

/// Configuration for spawning an augmented Node process.
pub struct SpawnConfig<'a> {
    /// The resolved Node binary.
    pub node: &'a ResolvedNode,
    /// User's original argv to pass to Node.
    pub user_args: &'a [String],
    /// Whether to skip all runtime augmentation (--node compat mode).
    pub compat_mode: bool,
    /// Nub's --show-warnings flag.
    pub show_warnings: bool,
    /// Path to the Nub binary itself (for the PATH shim).
    pub nub_binary: &'a Path,
    /// Parsed .env vars to inject into the child environment.
    pub env_vars: &'a std::collections::HashMap<String, String>,
    /// Yarn PnP `.pnp.cjs` path (from `nub_core::pnp::detect`), injected via
    /// `--require` ahead of nub's own preload so PnP's resolver patches install
    /// first. `None` when not in a PnP tree.
    pub pnp: Option<&'a std::path::Path>,
    /// Working directory for the spawned Node child. For `nub <file>` this is the
    /// process cwd (a no-op); the workspace-bin path threads each member's dir so
    /// a node bin run via `nub exec -r` executes IN the member, seeing its own
    /// `.env` / Node pin / `.bin` chain rather than the workspace root's.
    pub cwd: &'a Path,
}

/// The result of spawning a Node process.
pub struct SpawnResult {
    pub status: ExitStatus,
}

/// Spawn Node with Nub's augmentation pipeline.
///
/// In compat mode, spawns Node with only the user's args — no flag
/// injection, no preloads, no PATH shim.
pub fn spawn_node(config: &SpawnConfig<'_>) -> Result<SpawnResult> {
    let mut cmd = Command::new(config.node.path.as_str());
    // Process-identity fidelity: set argv0 to "node" so the spawned process
    // reports `process.title` and `process.argv0` as "node" — matching what
    // plain `node` reports when invoked by PATH name — instead of the full
    // resolved binary path that Rust passes by default. `process.execPath`
    // is NOT affected: it is populated by Node from the resolved binary path
    // (via `/proc/self/exe` on Linux, `_NSGetExecutablePath` on macOS) and
    // ignores argv0 entirely.
    //
    // Unix-only — and this is a hard platform boundary, not just a missing API:
    //   * Rust's `CommandExt::arg0` exists only on Unix; Windows passes a single
    //     command-line string whose token[0] is, by universal launcher
    //     convention, the executable path — there is no separate argv0 channel
    //     to override.
    //   * Even if there were, Node's `process.title` on Windows is NOT
    //     argv0-derived: libuv's `uv_get_process_title` reads
    //     `GetModuleFileNameW(NULL)` (the OS image path), so it is always the
    //     absolute `node.exe` path regardless of how the child was launched.
    // Crucially, plain Windows `node` reports that same full path for both
    // `process.title` and `process.argv0`, so nub does NOT diverge from Node on
    // Windows — there is nothing to fix there, and nothing the spawner could do
    // to force "node". (See crates/nub-cli/tests/process_identity.rs, which
    // asserts the Unix "node" invariant and the Windows path-passthrough one.)
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.arg0("node");
    }
    // Run the child in the configured cwd. For `nub <file>` this equals the
    // process cwd (a no-op); the workspace-bin path threads a member dir so a
    // node bin executes IN that member rather than inheriting the parent's cwd.
    cmd.current_dir(config.cwd);

    // Permission model detection and auto-grant.
    let has_permission = config.user_args.iter().any(|a| is_permission_flag(a));
    let has_allow_addons = config.user_args.iter().any(|a| a == "--allow-addons");

    if has_permission && !has_allow_addons && !config.compat_mode {
        anyhow::bail!(
            "nub: --permission requires --allow-addons\n\
             \x20\x20Nub's transpiler uses a native addon (oxc-transform).\n\
             \x20\x20Add --allow-addons to your Node permission flags,\n\
             \x20\x20or use --node to run without Nub's augmentation."
        );
    }

    if has_permission && !config.compat_mode {
        // Auto-grant read access to Nub's install directory.
        let install_dir = config
            .nub_binary
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(config.nub_binary);
        cmd.arg(format!("--allow-fs-read={}", install_dir.display()));
    }

    // ShimGuard must live until after child.wait() — declared at function scope.
    let mut _shim_guard: Option<ShimGuard> = None;
    // Removes the compile-cache sentinel (R8) on drop, after the child exits.
    let mut _ccache_guard: Option<CompileCacheSentinelGuard> = None;

    // Our preload path is both the re-entrancy key and the thing we inject, so
    // resolve it once up front. Detect a re-entrant invocation (a parent nub
    // already augmented this process tree via the PATH shim) by checking
    // NODE_OPTIONS for OUR specific preload path — not a generic "preload.mjs"
    // substring, which would false-positive on a user's own `--import` of an
    // unrelated file named preload.mjs and silently disable augmentation (A26).
    let preload = find_preload(config.nub_binary);
    // The injected form is tier-specific: `--require <path>` (fast tier, CJS
    // preload.cjs) or `--import <url>` (compat tier, ESM preload.mjs). Re-entrancy
    // is detected by finding that exact `--flag=value` token in the child's
    // inherited NODE_OPTIONS — so key the check on the token nub actually injects,
    // not a bare path/URL. (On Windows the URL has forward slashes and a stripped
    // prefix; token-keying keeps the parent/child match consistent across both
    // tiers and platforms, and still can't false-positive on a user's unrelated
    // preload.mjs — A26.)
    let injection = preload
        .as_deref()
        .map(|p| preload_injection(p, &config.node.version));
    let reentrancy_key = injection.as_ref().map(|i| i.node_options_token());
    let is_reentrant = is_reentrant_in(
        env::var("NODE_OPTIONS").ok().as_deref(),
        reentrancy_key.as_deref(),
    );

    // Augment only when we can locate our own preload. If `find_preload` fails —
    // a broken install, or (Windows, A-WIN2) the PATH-shim `node.exe` running
    // from a temp dir where the relative walk to `runtime/` can't reach (a
    // hardlink/copy, unlike a unix symlink, doesn't canonicalize back to the
    // install dir) — there is nothing to inject. Pass through instead: the child
    // inherits the parent's NODE_OPTIONS (absolute preload path) + PATH shim,
    // which already carry the augmentation, so re-augmenting here would only add
    // a half-setup (flags + a nested shim, no preload). See
    // wiki/runtime/hijack-by-default.md.
    if !config.compat_mode && !is_reentrant && preload.is_some() {
        // Flag injection.
        let node_options = env::var("NODE_OPTIONS").ok();
        let inject = flags::compute_inject_flags(
            config.node.version.clone(),
            config.user_args,
            node_options.as_deref(),
            config.show_warnings,
        );
        for flag in &inject {
            cmd.arg(flag);
        }

        // Web Storage: nub ALWAYS injects `--experimental-webstorage` on the band
        // where that flag is the enabling mechanism (Node 22.4 through <25, i.e.
        // `webstorage_flag_needed`), regardless of whether the user opted into
        // localStorage persistence (the maintainer, 2026-06-15: "a flag that we inject no
        // matter what"). On that band `sessionStorage` needs ONLY the flag (no file)
        // — gating it behind a `--localstorage-file` opt-in wrongly broke out-of-the-
        // box sessionStorage. So inject the flag unconditionally in-band; this makes
        // sessionStorage work everywhere on 22.4–24 and installs the `localStorage`
        // getter (which still throws `ERR_INVALID_ARG_VALUE` on ACCESS until the user
        // supplies a `--localstorage-file`). Empirically the flag alone does NOT throw
        // at startup on 22.4–24, so always-injecting is safe.
        //
        // nub NEVER synthesizes `--localstorage-file` — localStorage persistence
        // stays the user's explicit opt-in (forwarded verbatim if they pass it).
        //
        // Scope is exactly the `webstorage_flag_needed` band: below 22.4 the flag is
        // an unrecognized "bad option" (would crash startup), and on 25+ Web Storage
        // is native so the flag is unnecessary. Skip the inject when the user already
        // supplied `--experimental-webstorage` / `--no-experimental-webstorage` (no
        // double-add; respect an explicit disable — nub never re-enables over a user
        // negation).
        if should_inject_webstorage_flag(
            &config.node.version,
            config.user_args,
            node_options.as_deref(),
        ) {
            cmd.arg("--experimental-webstorage");
        }

        // PATH shim: prepend a temp dir with a `node` symlink → nub.
        if let Ok(shim_dir) = setup_path_shim(config.nub_binary) {
            let mut new_path = std::ffi::OsString::from(shim_dir.as_str());
            if let Some(existing) = env::var_os("PATH") {
                new_path.push(crate::PATH_LIST_SEPARATOR);
                new_path.push(existing);
            }
            cmd.env("PATH", new_path);
            _shim_guard = Some(ShimGuard {
                path: PathBuf::from(shim_dir.as_str()),
            });
        }

        // Yarn PnP: `--require <.pnp.cjs>` BEFORE nub's own preload so PnP's
        // `_resolveFilename` + zipfs patches install first; nub's resolve hooks
        // then layer on top. Inside the `!compat_mode && !is_reentrant` gate, so
        // `--node` and re-entrant child shells both skip PnP for free.
        if let Some(pnp) = config.pnp {
            cmd.arg("--require").arg(pnp);
        }

        // Preload injection: `--require <cjs-path>` (fast tier) or `--import <url>`
        // (compat tier). See PreloadInjection for why the channel is tier-specific.
        if let Some(ref inj) = injection {
            cmd.arg(inj.flag);
            cmd.arg(&inj.value);
        }

        // Coverage-exclude nub's own runtime (R9). When the user runs the test
        // runner under `--experimental-test-coverage`, Node instruments every
        // module it loads — including nub's preloaded runtime/*.mjs — and folds
        // them into the user's coverage report, tanking the aggregate (a 100% TS
        // fixture drops to ~55%) and adding phantom rows. Node accepts MULTIPLE
        // `--test-coverage-exclude=<glob>` flags, so we add one more keyed to the
        // ABSOLUTE nub runtime dir (the directory holding the preload we just
        // injected) — never a broad `**/runtime/**`, which would also exclude a
        // user's own `runtime/` source.
        if flags::test_coverage_exclude_supported(&config.node.version) {
            if let Some(glob) = coverage_exclude_glob(
                config.user_args,
                node_options.as_deref(),
                preload.as_deref(),
            ) {
                cmd.arg(glob);
            }
        }

        // Compile-cache pollution fix (R8). When the user sets NODE_COMPILE_CACHE
        // (or NODE_OPTIONS carries --use-compile-cache), Node enables the V8 code
        // cache AT BOOTSTRAP — *before* the user entry — so every module nub's
        // `--require` preload chain pulls in (preload.cjs, transform-core.mjs,
        // preload-common.cjs, polyfills.cjs, …) gets compiled-and-cached into the
        // USER's cache dir. A program that does `fs.readdirSync(NODE_COMPILE_CACHE)`
        // then sees ~9 nub entries instead of its own 1 (program-observable).
        //
        // Fix: STRIP NODE_COMPILE_CACHE from the child env so bootstrap caches
        // NOTHING, then hand the original dir to the preload through a non-env
        // sentinel file (brand rule: no NUB_* env var, and we must not leave the
        // var visible in the child's `printenv`). The preload, AFTER all nub setup
        // and right before user code, calls `module.enableCompileCache(dir)` so the
        // user's OWN modules still cache into their dir — the feature keeps working,
        // only the preload chain is excluded. The sentinel path is keyed on nub's
        // PID; the child reads it from `process.ppid` (nub is its direct parent),
        // the same PID-keyed temp pattern as the PATH shim.
        //
        // COVERAGE GATE (compile-cache vs V8 coverage). A WARM compile cache makes
        // V8's coverage imprecise: cached bytecode collapses/omits per-branch ranges,
        // so under `--experimental-test-coverage` / NODE_V8_COVERAGE the line/branch
        // percentages inflate and ranges collapse vs plain node — silently. So when
        // THIS nub invocation is itself collecting coverage (flag in argv/NODE_OPTIONS,
        // or NODE_V8_COVERAGE in env — same signal coverage_exclude_glob keys on, plus
        // the env var), set up NO compile cache at all: no default dir, and don't honor
        // a user-set one for this run either (coverage precision wins over their cache;
        // it's a single coverage run). The complementary case — a coverage child that
        // nub's OWN spawn path never sees because the user's test code spawns it
        // directly — is handled in the preload (reenableUserCompileCache sets
        // NODE_DISABLE_COMPILE_CACHE=1 so descendants boot cache-off).
        let node_v8_coverage = env::var("NODE_V8_COVERAGE").ok();
        let coverage = coverage_active_for_cache(
            config.user_args,
            node_options.as_deref(),
            node_v8_coverage.as_deref(),
        );
        if let Some(dir) = env::var("NODE_COMPILE_CACHE")
            .ok()
            .filter(|s| !s.is_empty())
        {
            // A user-set NODE_COMPILE_CACHE is honored ALWAYS — including under
            // coverage (the maintainer, 2026-06-11: an explicit user flag clobbers any
            // default nub sets; their coverage numbers may be cache-affected, the
            // same tradeoff they'd have on plain node). Normal R8 strip+sentinel.
            cmd.env_remove("NODE_COMPILE_CACHE");
            if write_compile_cache_sentinel(&dir).is_ok() {
                _ccache_guard = Some(CompileCacheSentinelGuard);
            }
        } else if coverage {
            // No user cache + coverage active: suppress nub's DEFAULT compile
            // cache (a warm V8 cache collapses/omits per-branch coverage ranges,
            // silently inflating `--experimental-test-coverage` / NODE_V8_COVERAGE
            // numbers vs plain node). Drop any empty-string env and write no
            // sentinel, so the preload's restore finds nothing. Mirrored in the JS
            // half (preload-common.cjs reenableUserCompileCache) for coverage
            // children nub's spawn path never sees.
            cmd.env_remove("NODE_COMPILE_CACHE");
        } else if let Some(dir) = default_compile_cache_dir() {
            // Default-on compile cache (decided 2026-06-10, measured): when the
            // user hasn't set NODE_COMPILE_CACHE, point it at a nub-owned dir.
            // Big single-file bundles gain tens of ms per invocation (pnpm −70ms,
            // typescript.js −67ms — verified working through nub's full hook chain
            // via NODE_DEBUG_NATIVE=COMPILE_CACHE: blobs accepted on read, persist
            // skipped when unchanged); small graphs measure at noise, and a stale/
            // incompatible blob is validated-and-rejected by V8, never trusted.
            //
            // Route it through the SAME strip+sentinel dance as the user-set branch
            // (not a bare `cmd.env(NODE_COMPILE_CACHE, dir)`). Leaving the dir in the
            // child env meant EVERY descendant — including a coverage child the user's
            // test code spawns directly (`spawnSync(execPath, [fixtureWithCoverage])`),
            // which nub's own spawn path never sees — inherited it and enabled the
            // cache AT BOOTSTRAP, before any preload could gate it, collapsing that
            // child's V8 coverage ranges (the test-runner coverage-width snapshot
            // tests). With the sentinel, NODE_COMPILE_CACHE is absent from the child
            // env, so nothing boots cache-warm; each nub-preloaded process re-enables
            // the cache post-bootstrap via reenableUserCompileCache, which SKIPS the
            // re-enable (and sets NODE_DISABLE_COMPILE_CACHE=1 for its own descendants)
            // when that process is collecting coverage. Cost: the preload chain itself
            // is no longer bootstrap-cached on the default path — but that chain was
            // never the perf target (big user bundles are), and not caching nub's own
            // modules is strictly better for the R8 pollution invariant too.
            // Escape hatches unchanged: NODE_COMPILE_CACHE yourself, or
            // NODE_DISABLE_COMPILE_CACHE=1 (honored by Node).
            cmd.env_remove("NODE_COMPILE_CACHE");
            if let Some(dir) = dir.to_str() {
                if write_compile_cache_sentinel(dir).is_ok() {
                    _ccache_guard = Some(CompileCacheSentinelGuard);
                }
            }
        }

        // Dual-channel injection: set NODE_OPTIONS so hardcoded-path `node`
        // invocations inherit the preload + flags. We only reach here when NOT
        // re-entrant — i.e. NODE_OPTIONS does not already carry our preload — so
        // always (re)build it, appending any pre-existing NODE_OPTIONS. (The old
        // `already_injected` guard checked the same full path and is subsumed by
        // `is_reentrant` above.)
        let existing_opts = env::var("NODE_OPTIONS").ok().filter(|s| !s.is_empty());
        let mut node_opts_parts: Vec<String> = Vec::new();
        for flag in &inject {
            node_opts_parts.push(flag.to_string());
        }
        // Yarn PnP token BEFORE nub's preload token, mirroring the argv order
        // above so hardcoded-path `node` invocations inherit PnP-first ordering.
        // Quoted so a `.pnp.cjs` under a spacey path survives the tokenizer.
        if let Some(pnp) = config.pnp {
            node_opts_parts.push(format!(
                "--require={}",
                node_options_quote(&pnp.display().to_string())
            ));
        }
        if let Some(ref inj) = injection {
            node_opts_parts.push(inj.node_options_token());
        }
        // Coverage-exclude nub's own runtime (R9) — via NODE_OPTIONS, not just argv.
        // The CLI-arg form at the `cmd.arg(glob)` site above only reaches the DIRECT
        // child nub spawns. But the test-runner coverage fixtures spawn the actual
        // coverage child via `process.execPath` (their own `spawnSync`), which nub
        // never sees: that grandchild inherits nub's preload ONLY through NODE_OPTIONS
        // (carrying `--require=runtime/preload.cjs`), so without the exclude flag ALSO
        // in NODE_OPTIONS, nub's runtime modules get instrumented into the user's
        // coverage report — phantom rows + a skewed `all files` aggregate.
        //
        // It is NOT gated on `coverage_active`: the parent nub can't observe that the
        // grandchild will enable coverage (the flag lives in the fixture's own argv),
        // so we inject the exclude whenever a preload is present AND the target Node
        // actually has the flag. Node >= 22.5 accepts `--test-coverage-exclude` in
        // NODE_OPTIONS and treats it as a harmless no-op when coverage is off; below
        // 22.5 the flag does not exist and is REJECTED in NODE_OPTIONS ("not allowed
        // in NODE_OPTIONS"), aborting every nub invocation before it runs a line — so
        // it must be version-gated exactly like --disable-warning / webstorage.
        if flags::test_coverage_exclude_supported(&config.node.version) {
            if let Some(ref p) = preload {
                if let Some(runtime_dir) = Path::new(p).parent() {
                    // Quote the glob value: the runtime dir can sit under a spacey
                    // install path (Windows `Program Files`, macOS `Application
                    // Support`); an unquoted space would split the flag and either
                    // abort ("not allowed in NODE_OPTIONS" on the fragment) or
                    // silently drop the exclude.
                    node_opts_parts.push(format!(
                        "--test-coverage-exclude={}",
                        node_options_quote(&format!("{}/**", runtime_dir.display()))
                    ));
                }
            }
        }
        // Web Storage (mirrors the argv site above): always inject
        // `--experimental-webstorage` into NODE_OPTIONS on the flag-needed band
        // (22.4–24.x), regardless of any `--localstorage-file` opt-in, so a child
        // `node` re-invocation inherits the flag and `sessionStorage` works out of
        // the box. nub never synthesizes `--localstorage-file`. Same guard: only
        // in-band, and not if the user already supplied/disabled the flag.
        if should_inject_webstorage_flag(
            &config.node.version,
            config.user_args,
            node_options.as_deref(),
        ) {
            node_opts_parts.push("--experimental-webstorage".to_string());
        }
        if let Some(existing) = existing_opts {
            node_opts_parts.push(existing);
        }
        if !node_opts_parts.is_empty() {
            cmd.env("NODE_OPTIONS", node_opts_parts.join(" "));
        }

        // NODE_PATH so the transpile's bare helper requires (e.g.
        // `@oxc-project/runtime/helpers/decorate` for decorators) resolve to
        // nub's vendored runtime deps. The ESM-import form is handled by the
        // resolve hook (VENDORED_PACKAGES), but a CJS `require()` bypasses the
        // hook and uses Node's native resolver, which only finds them via
        // NODE_PATH. No-op in dev (runtime/ has no node_modules → walk-up to the
        // repo's), active for an installed package (A30).
        if let Some(node_path) = vendored_node_path(preload.as_deref()) {
            cmd.env("NODE_PATH", node_path);
        }
    }

    // .env vars injected by the CLI layer.
    for (k, v) in config.env_vars {
        cmd.env(k, v);
    }

    // User args always pass through.
    cmd.args(config.user_args);

    // Inherit stdio.
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {}", config.node.path))?;

    // Forward Ctrl-C to the child so it reaches dev servers. Registered once;
    // the current target lives in a global atomic (see `ctrl_c`). The file-run
    // child IS `node` (the leaf), so a positive single-pid target is correct —
    // the script path uses group targeting because an `sh -c` sits in the middle.
    #[cfg(unix)]
    ctrl_c::track(child.id() as i32);

    let status = child.wait().with_context(|| "waiting for Node child")?;

    // Stop forwarding to this (now-exited) pid before returning.
    #[cfg(unix)]
    ctrl_c::untrack();

    Ok(SpawnResult { status })
}

/// RAII guard that removes the PATH shim directory on drop.
pub struct ShimGuard {
    path: PathBuf,
}

impl Drop for ShimGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Path of the compile-cache sentinel file (R8) for a given nub PID. spawn.rs
/// writes the user's original `NODE_COMPILE_CACHE` dir here keyed on nub's own
/// PID; the child preload reads it from a path derived from `process.ppid` (nub
/// is the child's direct parent). Same `<tmpdir>/nub-…-<pid>` shape as the PATH
/// shim, so cleanup is symmetric and a recycled PID can't collide across runs.
fn compile_cache_sentinel_path(nub_pid: u32) -> PathBuf {
    compile_cache_tmpdir().join(format!("nub-ccache-{nub_pid}"))
}

/// The temp dir for the compile-cache sentinel, resolved to MATCH the JS side's
/// `tmpdirNoOs()` (preload-common.cjs) so both ends agree on the path. Both must
/// resolve identically or the child can't find the sentinel nub wrote — which
/// silently disables the compile cache (the symptom: the default cache never
/// populates when TMPDIR is unset). We deliberately do NOT use `env::temp_dir()`:
/// on macOS it returns the per-user Darwin confstr dir (`/var/folders/.../T`) even
/// when TMPDIR is unset, whereas Node's `os.tmpdir()` falls back to `/tmp` — so the
/// two disagree in a clean (`env -i`) environment, exactly the case the corpus
/// harness spawns under (it forwards only PATH + HOME, not TMPDIR). Mirror Node's
/// libuv resolution: POSIX TMPDIR→TMP→TEMP→/tmp, Win32 TEMP→TMP→SystemRoot\temp,
/// trailing-separator-stripped — identical to tmpdirNoOs(). nub forwards its own
/// env to the child, so resolving from nub's env vars yields the child's view.
fn compile_cache_tmpdir() -> PathBuf {
    // Read the live process env; the resolution logic is pure over a lookup so it can
    // be table-tested without mutating process env (see compile_cache_tmpdir_from).
    compile_cache_tmpdir_from(|k| env::var(k).ok().filter(|s| !s.is_empty()))
}

/// Pure resolver behind [`compile_cache_tmpdir`]: given an env lookup that returns
/// `None` for unset/empty, reproduce Node's libuv `os.tmpdir()` env resolution
/// (POSIX: TMPDIR→TMP→TEMP→/tmp; Win32: TEMP→TMP→SystemRoot/windir+\temp),
/// trailing-separator-stripped. Kept byte-parity with the JS `tmpdirNoOs()`
/// (preload-common.cjs) so both ends agree on the sentinel path. Injectable so the
/// table test never touches process env (the suite runs tests in parallel).
fn compile_cache_tmpdir_from(lookup: impl Fn(&str) -> Option<String>) -> PathBuf {
    let strip_trailing = |mut s: String, sep: char| -> String {
        if s.len() > 1 && s.ends_with(sep) && !s.ends_with(&format!(":{sep}")) {
            s.pop();
        }
        s
    };
    if cfg!(windows) {
        let dir = lookup("TEMP").or_else(|| lookup("TMP")).unwrap_or_else(|| {
            let root = lookup("SystemRoot")
                .or_else(|| lookup("windir"))
                .unwrap_or_default();
            format!("{root}\\temp")
        });
        return PathBuf::from(strip_trailing(dir, '\\'));
    }
    let dir = lookup("TMPDIR")
        .or_else(|| lookup("TMP"))
        .or_else(|| lookup("TEMP"))
        .unwrap_or_else(|| "/tmp".to_string());
    PathBuf::from(strip_trailing(dir, '/'))
}

/// Write the user's original compile-cache dir to this nub process's sentinel
/// file. The preload reads + deletes it, then calls
/// `module.enableCompileCache(dir)` so the user's own modules cache into their
/// dir while nub's stripped-out preload chain never does (R8). Best-effort: a
/// write failure just means the child won't re-enable compile cache (no
/// pollution either way, since we've already stripped the env var).
fn write_compile_cache_sentinel(dir: &str) -> std::io::Result<()> {
    fs::write(compile_cache_sentinel_path(std::process::id()), dir)
}

/// Removes this process's compile-cache sentinel on drop (R8). The preload
/// deletes it on read in the common case; this guard reclaims it if the child
/// exited before reading (early crash, bad flag) so we never leak the file.
struct CompileCacheSentinelGuard;

impl Drop for CompileCacheSentinelGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(compile_cache_sentinel_path(std::process::id()));
    }
}

fn setup_path_shim(nub_binary: &Path) -> Result<Utf8PathBuf> {
    let pid = std::process::id();
    let dir_name = format!("nub-node-shim-{pid}");
    let shim_dir = env::temp_dir().join(&dir_name);

    fs::create_dir_all(&shim_dir)
        .with_context(|| format!("creating PATH shim dir: {}", shim_dir.display()))?;

    #[cfg(unix)]
    let node_shim = shim_dir.join("node");
    #[cfg(windows)]
    let node_shim = shim_dir.join("node.exe");

    // Concurrent workspace spawns (one nub process, N worker threads — see the
    // run -r work queue in cli.rs) race this setup, so the shim must be
    // PUBLISHED ATOMICALLY: materialize under a unique temp name, then rename
    // into place. A racer can then only ever observe `node`/`node.exe` absent
    // or complete. The naive `exists()`-then-create had two real failure modes:
    // on Windows the fallback `fs::copy` (hard_link fails across volumes — on
    // GitHub runners the repo is on D:, TEMP on C:) leaves a half-written
    // `node.exe` open for write that a sibling's child shell then tries to
    // EXECUTE → ERROR_SHARING_VIOLATION ("The process cannot access the file
    // because it is being used by another process", the run_aggregate CI
    // flake, 2026-06-10); on Unix two threads could both pass `!exists()` and
    // the loser's `symlink` EEXIST error silently dropped the shim via the
    // caller's `.ok()`. Losing the rename race is fine — the winner's shim is
    // complete by definition; clean up our temp and use theirs.
    if !node_shim.exists() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TMP_N: AtomicU64 = AtomicU64::new(0);
        let tmp = shim_dir.join(format!(
            ".node-staging-{pid}-{}",
            TMP_N.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(unix)]
        {
            unix_fs::symlink(nub_binary, &tmp)
                .with_context(|| format!("creating node shim symlink in {}", shim_dir.display()))?;
        }
        #[cfg(windows)]
        {
            fs::hard_link(nub_binary, &tmp)
                .or_else(|_| fs::copy(nub_binary, &tmp).map(|_| ()))
                .with_context(|| {
                    format!(
                        "creating node shim in {} (tried hard_link then copy)",
                        shim_dir.display()
                    )
                })?;
        }
        if let Err(rename_err) = fs::rename(&tmp, &node_shim) {
            let _ = fs::remove_file(&tmp);
            // A sibling published first (their shim is complete) — otherwise
            // the rename failed for a real reason worth surfacing.
            if !node_shim.exists() {
                return Err(rename_err)
                    .with_context(|| format!("publishing node shim into {}", shim_dir.display()));
            }
        }
    }

    Utf8PathBuf::try_from(shim_dir).map_err(|e| anyhow::anyhow!("shim dir path not UTF-8: {e}"))
}

/// Compute the augmentation environment variables (NODE_OPTIONS + PATH)
/// that script runners need to set on child shells so that `node` invocations
/// inside scripts get nub's transpilation, polyfills, and flag injection — the
/// same augmentation `nub <file>` applies via direct args. (Web Storage is
/// opt-in and never injected here; see `spawn_node`.)
///
/// Returns `None` if already re-entrant (parent nub already set up augmentation)
/// or if compat mode is active.
///
/// The PATH shim temp dir created here is process-wide (keyed by PID) and
/// reclaimed exactly once on process exit via [`cleanup_shim`]; it is
/// deliberately NOT returned as a per-call RAII guard, because concurrent
/// workspace scripts share the one dir and a per-call drop would `rm -rf` it
/// out from under sibling scripts still running.
pub fn compute_augmentation_env(
    nub_binary: &Path,
    node_version: super::version::NodeVersion,
    compat_mode: bool,
    pnp: Option<&Path>,
) -> Option<AugmentationEnv> {
    if compat_mode {
        return None;
    }

    // Bail if a parent nub already augmented this process tree, detected by OUR
    // specific preload path in NODE_OPTIONS (not a "preload.mjs" substring, which
    // would false-positive on a user's unrelated preload.mjs — A26).
    let preload = find_preload(nub_binary);
    // Key re-entrancy on the tier-specific injection token nub actually emits
    // (`--require=<cjs>` fast / `--import=<url>` compat), not a bare path (see
    // spawn_node + preload_injection).
    let injection = preload
        .as_deref()
        .map(|p| preload_injection(p, &node_version));
    let reentrancy_key = injection.as_ref().map(|i| i.node_options_token());
    if is_reentrant_in(
        env::var("NODE_OPTIONS").ok().as_deref(),
        reentrancy_key.as_deref(),
    ) {
        return None;
    }
    // Nothing to inject if we can't locate our preload (broken install, or a
    // Windows temp PATH-shim that can't walk back to runtime/ — A-WIN2): pass
    // through so the child inherits the parent's already-augmented env.
    let preload = preload?;
    let injection = injection.expect("injection is Some when preload is Some");

    let existing_node_options = env::var("NODE_OPTIONS").ok().filter(|s| !s.is_empty());

    // Build NODE_OPTIONS. Unlike the direct-spawn path (which passes flags as
    // argv to `node`), scripts run under a shell, so EVERY flag must travel via
    // NODE_OPTIONS — injected experimental flags, the preload, and webstorage.
    // Dedupe injected flags against any existing NODE_OPTIONS so we don't emit a
    // flag the user already set.
    let inject = flags::compute_inject_flags(
        node_version.clone(),
        &[],
        existing_node_options.as_deref(),
        false,
    );
    let mut node_opts_parts: Vec<String> = inject.iter().map(|f| f.to_string()).collect();
    // Yarn PnP `--require <.pnp.cjs>` BEFORE nub's preload token so PnP's
    // resolver installs first in script-runner child shells too. Quoted: a
    // `.pnp.cjs` under a spacey project path would otherwise fragment.
    if let Some(pnp) = pnp {
        node_opts_parts.push(format!(
            "--require={}",
            node_options_quote(&pnp.display().to_string())
        ));
    }
    node_opts_parts.push(injection.node_options_token());
    // Web Storage (mirrors `spawn_node`): always inject
    // `--experimental-webstorage` on the flag-needed band (22.4–24.x) so a
    // script-run child shell's `node` has `sessionStorage` out of the box, with no
    // `--localstorage-file` opt-in required. nub never synthesizes
    // `--localstorage-file`. (Scripts have no argv here — the only user channel is
    // NODE_OPTIONS.) Guarded against double-add / a user
    // `--no-experimental-webstorage` disable.
    if should_inject_webstorage_flag(&node_version, &[], existing_node_options.as_deref()) {
        node_opts_parts.push("--experimental-webstorage".to_string());
    }
    if let Some(existing) = existing_node_options {
        node_opts_parts.push(existing);
    }

    let node_options = if node_opts_parts.is_empty() {
        None
    } else {
        Some(node_opts_parts.join(" "))
    };

    // The bare PATH-shim dir. Callers compose `shim_dir : node_modules/.bin :
    // existing PATH` — the shim first so child `node` hits nub-as-node, then the
    // walked-up `.bin` dirs BEFORE the system PATH so a locally-installed tool
    // shadows a global one (npm/pnpm parity; bundling `existing` into the shim
    // here used to push `.bin` after the system PATH, the A9-adjacent shadowing
    // bug). `existing` appears exactly once, supplied by `bin_path`.
    let shim_dir = setup_path_shim(nub_binary)
        .ok()
        .map(|d| d.as_str().to_string());

    Some(AugmentationEnv {
        node_options,
        shim_dir,
        node_path: vendored_node_path(Some(&preload)),
    })
}

/// Augmentation environment for script runners.
pub struct AugmentationEnv {
    pub node_options: Option<String>,
    /// The bare PATH-shim dir (NOT bundled with the system PATH). Callers prepend
    /// it ahead of `node_modules/.bin` + the system PATH.
    pub shim_dir: Option<String>,
    /// NODE_PATH so CJS `require()` of the transpile's vendored helper deps
    /// resolves from an installed package (A30). `None` in dev / when absent.
    pub node_path: Option<std::ffi::OsString>,
}

impl AugmentationEnv {
    /// The PATH-shim's `node` entry (a symlink/hardlink → nub), suitable as the
    /// `$NODE` value that npm/pnpm set so userland `$NODE child.js` (and
    /// `spawn(process.env.NODE, …)`) invoke "the same Node this script runs under."
    /// Pointing `$NODE` here — rather than the raw binary — makes an absolute-path
    /// `$NODE` re-enter nub and stay augmented, identical to a bare `node` (which
    /// reaches the shim via PATH). The shim is a faithful node front-end
    /// (`$NODE --version` prints Node's version; `process.execPath` still reports the
    /// real binary), so introspection is preserved. `None` when no shim was set up
    /// (then callers fall back to the real binary for plain npm/pnpm parity).
    pub fn node_shim_exe(&self) -> Option<std::ffi::OsString> {
        self.shim_dir.as_deref().map(|dir| {
            #[cfg(windows)]
            let name = "node.exe";
            #[cfg(not(windows))]
            let name = "node";
            Path::new(dir).join(name).into_os_string()
        })
    }
}

/// Node's permission-model flags — the *exact, closed* set that, when present,
/// engages Node's `--permission` sandbox (and therefore needs `--allow-addons`
/// for nub's native oxc-transform addon to dlopen). This MUST be an exact
/// allowlist, not a `starts_with("--allow-")` prefix match: V8 exposes flags that
/// share the `--allow-` prefix but are NOT permission flags — most notably
/// `--allow-natives-syntax` (enables `%`-prefixed V8 natives like
/// `%OptimizeFunctionOnNextCall`). The old prefix match misclassified it as a
/// permission flag and aborted `nub --allow-natives-syntax x.js` with
/// "--permission requires --allow-addons", where stock node runs it (exit 0).
/// `--allow-ffi` is a real Node permission flag on the versions that carry it
/// (it is gone on node 25, where node itself rejects it as a bad option) and is
/// deliberately kept here so it classifies correctly wherever it exists. Match
/// the token up to any `=`, since the value-taking flags appear as
/// `--allow-fs-read=/path`, `--allow-net=host`, etc.
fn is_permission_flag(arg: &str) -> bool {
    const PERMISSION_FLAGS: &[&str] = &[
        "--permission",
        "--allow-addons",
        "--allow-child-process",
        "--allow-ffi",
        "--allow-fs-read",
        "--allow-fs-write",
        "--allow-inspector",
        "--allow-net",
        "--allow-wasi",
        "--allow-worker",
    ];
    let token = arg.split('=').next().unwrap_or(arg);
    PERMISSION_FLAGS.contains(&token)
}

/// Whether the user already supplied the `--experimental-webstorage` flag in
/// either polarity (`--experimental-webstorage` or `--no-experimental-webstorage`)
/// via argv or NODE_OPTIONS. When true, nub must NOT add its own
/// `--experimental-webstorage`: a duplicate positive is redundant, and overriding a
/// user's explicit `--no-experimental-webstorage` would defeat their disable
/// (and nub never re-enables over a user negation). Pure over its inputs.
fn user_has_webstorage_flag(user_args: &[String], node_options: Option<&str>) -> bool {
    let is_ws = |t: &str| t == "--experimental-webstorage" || t == "--no-experimental-webstorage";
    let in_argv = user_args.iter().any(|a| is_ws(a));
    let in_opts = node_options
        .map(|o| o.split_whitespace().any(is_ws))
        .unwrap_or(false);
    in_argv || in_opts
}

/// Whether nub should inject `--experimental-webstorage` for this invocation
/// (the maintainer, 2026-06-15: "a flag that we inject no matter what"). True iff the Node
/// version is on the flag-needed band (22.4 through <25, where the flag both EXISTS
/// and is still REQUIRED) AND the user hasn't already supplied the flag in either
/// polarity. The inject is UNCONDITIONAL on the band — it does not depend on any
/// `--localstorage-file` opt-in — so `sessionStorage` works out of the box; it
/// installs the `localStorage` getter too (which throws on access until the user
/// supplies their own `--localstorage-file`; nub never synthesizes one). Below 22.4
/// the flag is a "bad option" startup crash; on 25+ Web Storage is native so the
/// flag is unnecessary. Pure over its inputs for testability.
fn should_inject_webstorage_flag(
    node_version: &super::version::NodeVersion,
    user_args: &[String],
    node_options: Option<&str>,
) -> bool {
    flags::webstorage_flag_needed(node_version)
        && !user_has_webstorage_flag(user_args, node_options)
}

/// Whether Node's test-runner coverage is active for this invocation — i.e. the
/// user passed `--experimental-test-coverage` directly in argv or via NODE_OPTIONS.
/// (`nub` has no separate coverage verb; coverage is engaged solely by that flag,
/// so detecting it in either channel is the complete trigger.)
fn coverage_active(user_args: &[String], node_options: Option<&str>) -> bool {
    let in_argv = user_args
        .iter()
        .any(|a| a == "--experimental-test-coverage");
    let in_opts = node_options
        .map(|o| {
            o.split_whitespace()
                .any(|t| t == "--experimental-test-coverage")
        })
        .unwrap_or(false);
    in_argv || in_opts
}

/// Whether V8 coverage is active for the compile-cache gate — the same
/// `--experimental-test-coverage` signal `coverage_active` keys on (argv +
/// NODE_OPTIONS), PLUS a non-empty `NODE_V8_COVERAGE` env. The extra env check is
/// what `coverage_active` (used only for the R9 exclude-glob, which is itself
/// keyed to the coverage *flag*) doesn't need but the cache gate does: a user can
/// engage coverage purely through `NODE_V8_COVERAGE=<dir>` with no flag, and a warm
/// compile cache corrupts that path's ranges just the same. A user-set
/// NODE_COMPILE_CACHE is intentionally NOT consulted here — see the call site for
/// why a coverage run overrides even an explicit cache dir.
fn coverage_active_for_cache(
    user_args: &[String],
    node_options: Option<&str>,
    node_v8_coverage: Option<&str>,
) -> bool {
    coverage_active(user_args, node_options) || node_v8_coverage.is_some_and(|v| !v.is_empty())
}

/// The `--test-coverage-exclude=<glob>` flag nub injects to keep its own preloaded
/// runtime modules out of the user's coverage report (R9), or `None` when coverage
/// isn't active or the runtime dir can't be resolved. The glob is keyed to the
/// ABSOLUTE directory holding the injected preload — the same dir `find_preload`
/// returns the preload from — so it can never accidentally match a user's own
/// `runtime/` directory the way a relative `**/runtime/**` would.
///
/// HONESTY CAVEAT: passing ANY `--test-coverage-exclude` perturbs Node's branch
/// baseline slightly — Node computes the total branch count over the set of files
/// it decides to report, so excluding files shifts the `all files` branch %
/// denominator a hair. This is a stock-Node quirk of `--test-coverage-exclude`,
/// NOT something nub introduces; a future reader comparing nub's aggregate to a
/// hand-computed one should not be surprised by a fractional branch-% difference.
fn coverage_exclude_glob(
    user_args: &[String],
    node_options: Option<&str>,
    preload: Option<&str>,
) -> Option<String> {
    if !coverage_active(user_args, node_options) {
        return None;
    }
    let runtime_dir = Path::new(preload?).parent()?;
    Some(format!(
        "--test-coverage-exclude={}/**",
        runtime_dir.display()
    ))
}

/// True when `node_options` already carries OUR specific preload path — i.e. a
/// parent nub set up augmentation for this process tree (a re-entrant invocation
/// reached through the PATH shim, whose `node` resolves back to nub). Matching
/// the full preload path, rather than a generic `"preload.mjs"` substring, means
/// a user's own `--import` of an unrelated file that happens to be named
/// `preload.mjs` is never mistaken for ours and cannot silently disable
/// augmentation (A26). Pure over its inputs so it is testable without touching
/// the process environment.
fn is_reentrant_in(node_options: Option<&str>, preload: Option<&str>) -> bool {
    match (node_options, preload) {
        (Some(opts), Some(preload)) => opts.contains(preload),
        _ => false,
    }
}

/// Strip Windows' verbatim / extended-length path prefixes (`\\?\` and
/// `\\?\UNC\`) that `fs::canonicalize` emits. Node's module loader and NODE_PATH
/// reject them. Returns a native Windows path (backslashes preserved — valid for
/// NODE_PATH and fs ops). Pure over `windows` so both branches test on any host.
fn strip_verbatim(path: &str, windows: bool) -> String {
    if windows {
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = path.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    path.to_string()
}

/// Convert a filesystem path to a `file://` URL Node's loader accepts on every
/// platform. On Windows a path is `C:\a\b` (or a canonicalized `\\?\C:\a\b`); a
/// naive `format!("file://{path}")` yields `file://C:\a\b`, which Node's
/// `fileURLToPath` rejects (ERR_INVALID_FILE_URL_PATH — the drive is parsed as the
/// URL authority and backslashes are invalid). Emit `file:///C:/a/b` for drive
/// paths and `file://server/share/...` for UNC. On Unix the path is already an
/// absolute forward-slash path, so `file://` + path gives the correct
/// `file:///abs/...`. Pure over `windows` so both branches test on any host.
fn to_file_url(path: &str, windows: bool) -> String {
    if !windows {
        return format!("file://{path}");
    }
    let forward = strip_verbatim(path, true).replace('\\', "/");
    if forward.starts_with("//") {
        // UNC: //server/share/... -> file://server/share/...
        format!("file:{forward}")
    } else {
        // Drive: C:/a/b -> file:///C:/a/b
        format!("file:///{forward}")
    }
}

/// Public path -> `file://` URL conversion for the current platform. Used wherever
/// nub injects `--import <url>` for the preload.
pub fn path_to_file_url(path: &str) -> String {
    to_file_url(path, cfg!(windows))
}

/// How nub injects its preload, chosen BY TIER. The fast tier (Node 22.15+) loads a
/// CommonJS preload via `--require`; the compat tier (18.19–22.14) loads the ESM
/// preload via `--import`. The channel choice is load-bearing: an `--import` ESM
/// preload forces eager ESM-loader init, which routes even a CJS entry through the
/// async ESM module-job and breaks Node's synchronous `Module.runMain` semantics
/// (top-level `executionAsyncId`, sync exception origin, `require.main.id`,
/// `module.parent`, missing-entry error code) — the R1 regression cluster. A
/// `--require` CJS preload keeps the sync entry path; on 22.15+ it can still
/// `module.registerHooks` + transpile TS. The compat tier has no reliable sync
/// surface (no `module.registerHooks`, `require(esm)` unreliable), so it keeps the
/// async `--import` path.
pub struct PreloadInjection {
    /// The flag introducing the preload: `--require` (fast) or `--import` (compat).
    pub flag: &'static str,
    /// The injected value: a raw path for `--require`, a `file://` URL for `--import`.
    pub value: String,
}

impl PreloadInjection {
    /// The single token form for NODE_OPTIONS (`--require=<v>` / `--import=<v>`),
    /// which doubles as the re-entrancy key: a child detects a parent-injected
    /// preload by finding this exact token in its inherited NODE_OPTIONS.
    ///
    /// The VALUE half is quoted with [`node_options_quote`] so a preload path
    /// containing a space (e.g. a cache or temp dir under `C:\Users\John Doe\…`,
    /// or a macOS `~/Library/Application Support/…`) survives Node's NODE_OPTIONS
    /// tokenizer, which splits on unquoted spaces. The re-entrancy detector
    /// ([`is_reentrant_in`]) compares against this same quoted form, so the key
    /// still round-trips.
    pub fn node_options_token(&self) -> String {
        format!("{}={}", self.flag, node_options_quote(&self.value))
    }
}

/// Quote a value for safe embedding in NODE_OPTIONS. Node's NODE_OPTIONS
/// tokenizer (`ParseNodeOptionsEnvVar`, .repos/node/src/node_options.cc:2214)
/// splits on spaces UNLESS the run is inside a double-quoted string, and treats
/// backslash as an escape ONLY inside such a string. So a value with a space
/// must be wrapped in `"…"`, and inside those quotes every `\` and `"` must be
/// backslash-escaped or the path corrupts (the load-bearing Windows case:
/// `C:\Users\John Doe\.cache` → without escaping, `\U`, `\J`, `\.` get eaten).
/// Single quotes do NOT work — Node has no single-quote handling, so they'd
/// become literal characters in the path (`ERR_INVALID_STATE` on the store).
///
/// Values WITHOUT a space are returned unchanged: they tokenize fine bare, and
/// not quoting them keeps NODE_OPTIONS readable and matches plain-Node argv.
/// Use this for EVERY value-bearing flag nub writes into NODE_OPTIONS
/// (`--test-coverage-exclude=`, the preload `--require=`/`--import=` token,
/// PnP `--require=`).
fn node_options_quote(value: &str) -> String {
    if value.contains(' ') {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

/// Pick the preload injection for a Node version, given the located ESM preload
/// path (`runtime/preload.mjs`). On the fast tier the sibling `runtime/preload.cjs`
/// is injected via `--require` (raw path — `require` takes a path, not a URL); on
/// the compat tier the `.mjs` is injected via `--import` (file:// URL). Pure over
/// `windows` for testability.
fn preload_injection_for(
    preload_mjs: &str,
    version: &super::version::NodeVersion,
    windows: bool,
) -> PreloadInjection {
    if version.supports_augmentation() {
        // Sibling .cjs in the same runtime dir. `--require` resolves a plain path
        // (it does NOT accept a file:// URL), so inject the raw path; verbatim
        // prefixes were already stripped by find_preload.
        let cjs = preload_mjs
            .strip_suffix(".mjs")
            .map(|stem| format!("{stem}.cjs"))
            .unwrap_or_else(|| preload_mjs.to_string());
        PreloadInjection {
            flag: "--require",
            value: cjs,
        }
    } else {
        PreloadInjection {
            flag: "--import",
            value: to_file_url(preload_mjs, windows),
        }
    }
}

/// Public wrapper over [`preload_injection_for`] for the current platform.
pub fn preload_injection(
    preload_mjs: &str,
    version: &super::version::NodeVersion,
) -> PreloadInjection {
    preload_injection_for(preload_mjs, version, cfg!(windows))
}

/// NODE_PATH value that makes nub's vendored runtime deps resolvable to a
/// CommonJS `require()` from transpiled output (A30). The transpile emits bare
/// helper imports (e.g. `@oxc-project/runtime/helpers/decorate` for decorators);
/// the ESM-import form resolves via the resolve hook (VENDORED_PACKAGES), but a
/// CJS `require()` bypasses the hook and uses Node's native resolver, which only
/// finds them through NODE_PATH. Returns `<preload-dir>/node_modules` prepended
/// to any existing NODE_PATH — but only when that dir exists (an installed
/// package). In dev `runtime/` has no `node_modules`, so this is None and the
/// requires resolve by walking up to the repo's `node_modules`, unchanged.
fn vendored_node_path(preload: Option<&str>) -> Option<std::ffi::OsString> {
    let vendored = Path::new(preload?).parent()?.join("node_modules");
    if !vendored.is_dir() {
        return None;
    }
    let mut value = vendored.into_os_string();
    if let Some(existing) = env::var_os("NODE_PATH").filter(|s| !s.is_empty()) {
        value.push(crate::PATH_LIST_SEPARATOR);
        value.push(existing);
    }
    Some(value)
}

/// Find the preload entry script relative to the Nub binary.
///
/// In development: `<repo>/runtime/preload.mjs`
/// In distribution: `<nub-install-dir>/runtime/preload.mjs`
pub fn find_public_preload(nub_binary: &Path) -> Option<String> {
    find_preload(nub_binary)
}

fn find_preload(nub_binary: &Path) -> Option<String> {
    // Walk up from the binary's directory to find runtime/preload.mjs.
    let mut dir = nub_binary.parent()?.to_path_buf();
    for _ in 0..5 {
        let candidate = dir.join("runtime").join("preload.mjs");
        if candidate.is_file() {
            // Strip the `\\?\` verbatim prefix `fs::canonicalize` adds on Windows so
            // the path is usable in NODE_PATH and convertible to a valid file:// URL.
            return candidate.to_str().map(|s| strip_verbatim(s, cfg!(windows)));
        }
        if !dir.pop() {
            break;
        }
    }
    tracing::warn!("preload not found relative to nub binary");
    None
}

/// Resolve the path to the currently running Nub binary (follows symlinks).
pub fn current_nub_binary() -> Result<PathBuf> {
    let exe = env::current_exe().context("could not determine path to nub binary")?;
    fs::canonicalize(&exe).or(Ok(exe))
}

/// Map a child's [`ExitStatus`] to a Unix-faithful process exit code: the normal
/// exit code when the child exited normally, or `128 + signal` when it was killed
/// by a signal (SIGTERM → 143, SIGINT → 130, SIGSEGV → 139) — matching what a
/// shell and plain `node` report. The previous `code().unwrap_or(1)` collapsed
/// every signal death to 1, discarding the signal. Non-Unix falls back to the
/// code or 1.
pub fn exit_code_from_status(status: &ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// Convert a Node [`SpawnResult`] to a process exit code (see
/// [`exit_code_from_status`]).
pub fn exit_code(result: &SpawnResult) -> i32 {
    exit_code_from_status(&result.status)
}

/// Clean up any PATH shim directories left by this process.
pub fn cleanup_shim() {
    let pid = std::process::id();
    let dir_name = format!("nub-node-shim-{pid}");
    let shim_dir = env::temp_dir().join(&dir_name);
    if shim_dir.exists() {
        let _ = fs::remove_dir_all(&shim_dir);
    }
}

/// The nub-owned default compile-cache dir (`<cache>/nub/v8-compile-cache`),
/// created best-effort. `None` when the cache root can't be resolved (no HOME) —
/// the spawn simply proceeds uncached, never errors.
pub fn default_compile_cache_dir() -> Option<std::ffi::OsString> {
    let dir = crate::node::discovery::cache_dir()?.join("v8-compile-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.into_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::version::NodeVersion;

    #[test]
    fn node_options_quote_only_wraps_spacey_values() {
        // No space → returned bare (tokenizes fine, stays readable / argv-like).
        assert_eq!(node_options_quote("/tmp/store.sqlite"), "/tmp/store.sqlite");
        // Space → wrapped in double quotes (single quotes are literal to Node's
        // tokenizer and would corrupt the path → ERR_INVALID_STATE).
        assert_eq!(
            node_options_quote("/tmp/nub cache/store.sqlite"),
            "\"/tmp/nub cache/store.sqlite\""
        );
        // Windows: backslashes inside the quotes are escape chars to Node, so each
        // must be doubled or `\U`/`\J`/`\.` get eaten. Only quoted when spacey.
        assert_eq!(
            node_options_quote(r"C:\Users\John Doe\.cache\store.sqlite"),
            r#""C:\\Users\\John Doe\\.cache\\store.sqlite""#
        );
        // A backslash path WITHOUT a space stays bare — Node only treats `\` as an
        // escape INSIDE a quoted string, so an unquoted backslash is literal.
        assert_eq!(
            node_options_quote(r"C:\Users\John\.cache\store.sqlite"),
            r"C:\Users\John\.cache\store.sqlite"
        );
        // An embedded double-quote in a spacey value is backslash-escaped.
        assert_eq!(node_options_quote(r#"/tmp/a "b" c"#), r#""/tmp/a \"b\" c""#);
    }

    #[test]
    fn webstorage_flag_always_injected_on_band_without_localstorage_file() {
        // the maintainer, 2026-06-15: nub injects --experimental-webstorage "no matter what"
        // on the flag-needed band (22.4–24), with NO --localstorage-file present —
        // so sessionStorage works out of the box. (a) in-band with no file → inject.
        for ver in [
            NodeVersion::new(22, 4, 0),
            NodeVersion::new(22, 15, 0),
            NodeVersion::new(24, 0, 0),
            NodeVersion::new(24, 99, 0),
        ] {
            assert!(
                should_inject_webstorage_flag(&ver, &[], None),
                "must inject --experimental-webstorage on {ver:?} with no --localstorage-file"
            );
        }
    }

    #[test]
    fn webstorage_flag_not_injected_below_floor_or_when_native() {
        // (b) below 22.4 the flag is an unrecognized "bad option" → never inject.
        for ver in [NodeVersion::new(18, 19, 0), NodeVersion::new(22, 3, 0)] {
            assert!(
                !should_inject_webstorage_flag(&ver, &[], None),
                "must NOT inject below the 22.4 floor ({ver:?}) — would crash startup"
            );
        }
        // (c) on 25+ Web Storage is native → the flag is unnecessary, don't inject.
        for ver in [NodeVersion::new(25, 0, 0), NodeVersion::new(26, 2, 0)] {
            assert!(
                !should_inject_webstorage_flag(&ver, &[], None),
                "must NOT inject on {ver:?} — Web Storage is native there"
            );
        }
    }

    #[test]
    fn webstorage_flag_not_double_injected_when_user_supplied() {
        // (e) user already passed the flag (either polarity, either channel) → nub
        // must not double-inject / must respect an explicit disable.
        let s = |v: &str| v.to_string();
        let v = NodeVersion::new(22, 15, 0);
        assert!(!should_inject_webstorage_flag(&v, &[s("--experimental-webstorage")], None));
        assert!(!should_inject_webstorage_flag(&v, &[], Some("--experimental-webstorage")));
        assert!(!should_inject_webstorage_flag(&v, &[s("--no-experimental-webstorage")], None));
        assert!(!should_inject_webstorage_flag(&v, &[], Some("--no-experimental-webstorage")));
        // A --localstorage-file opt-in does NOT change the in-band decision — the
        // flag injects either way; (d) nub never synthesizes --localstorage-file, so
        // its presence/absence is irrelevant to whether the flag is injected.
        assert!(should_inject_webstorage_flag(
            &v,
            &[s("--localstorage-file=/tmp/x.sqlite")],
            None
        ));
    }

    #[test]
    fn existing_user_webstorage_flag_suppresses_injection() {
        let s = |v: &str| v.to_string();
        // Neither polarity present → nub may inject.
        assert!(!user_has_webstorage_flag(&[s("app.js")], None));
        // User already passed the positive → don't double-add.
        assert!(user_has_webstorage_flag(&[s("--experimental-webstorage")], None));
        assert!(user_has_webstorage_flag(&[], Some("--experimental-webstorage")));
        // User explicitly disabled → respect it, never re-enable.
        assert!(user_has_webstorage_flag(
            &[s("--no-experimental-webstorage")],
            None
        ));
        assert!(user_has_webstorage_flag(
            &[],
            Some("--no-experimental-webstorage --localstorage-file=/tmp/x")
        ));
    }

    // `ctrl_c::CURRENT_CHILD` is a process-global AtomicU32. The two tests that
    // exercise it (`ctrl_c_forwards_*` and `diagnostic_signal_*`) therefore race
    // when cargo runs them on parallel threads — one test's `track(<real pid>)`
    // flips the global out from under the other's `current()` assertion (an
    // intermittent CI failure). Serialize them behind this guard. Poison-tolerant
    // so a panic in one doesn't cascade into a spurious failure in the other.
    static CTRL_C_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn exit_code_maps_signal_death_to_128_plus_signo() {
        // A child killed by a signal exits BY the signal (`.code()` is None,
        // `.signal()` is the signo), so `exit_code_from_status` must report
        // 128 + signo — SIGTERM => 143 — not collapse it to a generic 1.
        let killed = Command::new("sh")
            .arg("-c")
            .arg("kill -TERM $$")
            .status()
            .unwrap();
        assert_eq!(exit_code_from_status(&killed), 143);
        // A normal exit code passes through untouched.
        let normal = Command::new("sh").arg("-c").arg("exit 7").status().unwrap();
        assert_eq!(exit_code_from_status(&normal), 7);
    }

    #[cfg(unix)]
    #[test]
    fn ctrl_c_forwards_to_the_latest_child_not_the_first() {
        let _serial = CTRL_C_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // The bug A20 fixes: a second spawn's set_handler no-op'd, so the single
        // handler kept the first (dead) pid. Now the global pid updates per spawn,
        // so the handler always targets the current child; untrack clears it so a
        // stray SIGINT after exit is a no-op rather than a kill of a reused pid.
        ctrl_c::untrack(); // reset the shared global before asserting on it
        ctrl_c::track(111);
        assert_eq!(ctrl_c::current(), 111);
        ctrl_c::track(222);
        assert_eq!(
            ctrl_c::current(),
            222,
            "a later spawn must become the forwarded target"
        );
        ctrl_c::untrack();
        assert_eq!(
            ctrl_c::current(),
            0,
            "untrack clears the pid after the child exits"
        );
    }

    #[cfg(unix)]
    #[test]
    fn status_forwarding_signals_runs_then_clears_the_tracked_pid() {
        // The `nub run` script path routes through this instead of a raw
        // `command.status()` so docker stop / Ctrl-C reach the script child. It
        // must return the child's real status AND leave the global untracked, so a
        // stray signal after the script exits can't kill a reused pid.
        let _serial = CTRL_C_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ctrl_c::untrack();
        let status = status_forwarding_signals(Command::new("sh").arg("-c").arg("exit 7"))
            .expect("spawn sh");
        assert_eq!(
            exit_code_from_status(&status),
            7,
            "the child's code passes through"
        );
        assert_eq!(
            ctrl_c::current(),
            0,
            "the tracked pid is cleared once the child exits"
        );
    }

    #[cfg(unix)]
    #[test]
    fn group_targeting_stores_a_negative_pid() {
        // track_child_group must store `-pid` so the forwarder's kill(2) hits the
        // process GROUP (sh + the node it forks), not just sh — the orphan the
        // single-pid path left under a dash that forks its `sh -c` child.
        let _serial = CTRL_C_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ctrl_c::untrack();
        track_child_group(4321);
        assert_eq!(ctrl_c::current(), -4321, "group target is the negated pid");
        untrack_child();
        assert_eq!(ctrl_c::current(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_signal_reaches_child_and_parent_survives() {
        let _serial = CTRL_C_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // SIGUSR2 is the diagnostic-signal exemplar (Node's --report-signal default,
        // and what nodemon sends). Default disposition is TERMINATE the receiver, so
        // without nub installing a handler this signal would kill the resident parent
        // before the child ever saw it. This proves the two-part contract in one shot:
        //   (1) the child RECEIVES a relayed SIGUSR2 (it writes a marker file), and
        //   (2) the parent (this test process) SURVIVES — it keeps running past the
        //       signal to observe the marker, rather than being terminated by USR2.
        // A representative diagnostic signal covers the relay+survival contract; we
        // don't repeat per-signal (USR1/QUIT register through the identical path).
        let marker = env::temp_dir().join(format!(
            "nub-usr2-relay-{}-{}.marker",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_file(&marker);

        // A child that, on SIGUSR2, writes the marker and exits 0; otherwise sleeps.
        // It blocks (`wait`) on a background sleep so the trap can fire promptly.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap 'echo got >{m}; exit 0' USR2; sleep 5 & wait",
                m = marker.display()
            ))
            .spawn()
            .expect("spawn signal-trap child");

        // Register nub's forwarder for this child (installs the SIGUSR2 handler that
        // overrides the parent's terminate-on-USR2 default and relays to the child).
        ctrl_c::track(child.id() as i32);

        // Give the child's `trap` a moment to install before we deliver the signal.
        std::thread::sleep(std::time::Duration::from_millis(150));

        // Send SIGUSR2 to OURSELVES. If nub hadn't installed a handler, this line
        // would terminate the test binary (USR2's default action) and the test would
        // be recorded as a signal death — never reaching the assertions below.
        unsafe {
            libc::kill(std::process::id() as i32, libc::SIGUSR2);
        }

        // The relay is async (signal-hook self-pipe → forwarder thread → kill child),
        // so poll for the marker / child exit rather than racing it.
        let status = loop_wait(&mut child, std::time::Duration::from_secs(5));
        ctrl_c::untrack();

        let marker_written = marker.exists();
        let _ = fs::remove_file(&marker);

        assert!(
            marker_written,
            "child must have received the relayed SIGUSR2 and written its marker"
        );
        // The child exits 0 from its own trap — proving it ran ITS handler, not that
        // it was hard-killed.
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "child must exit 0 via its own SIGUSR2 trap"
        );
        // Reaching here at all is the parent-survival half: a process killed by USR2
        // never runs these assertions.
    }

    /// Wait up to `timeout` for `child` to exit, polling so an async relay has time
    /// to land. Returns the exit status, or None on timeout (then kills the child so
    /// the test never leaks a process).
    #[cfg(unix)]
    fn loop_wait(
        child: &mut std::process::Child,
        timeout: std::time::Duration,
    ) -> Option<ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Some(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    #[test]
    fn compile_cache_sentinel_round_trips_and_cleans_up() {
        // R8: spawn.rs hands the user's NODE_COMPILE_CACHE dir to the child preload
        // through a PID-keyed sentinel file (never a NUB_* env var). Prove the
        // write lands at the path the child derives from process.ppid, carries the
        // exact dir bytes, and that the guard reclaims it on drop (the early-exit
        // fallback for when the preload didn't consume it).
        let dir = "/tmp/some/user compile cache";
        write_compile_cache_sentinel(dir).unwrap();
        let path = compile_cache_sentinel_path(std::process::id());
        assert_eq!(fs::read_to_string(&path).unwrap(), dir);

        drop(CompileCacheSentinelGuard);
        assert!(
            !path.exists(),
            "the guard must remove the sentinel so it never leaks"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn compile_cache_tmpdir_mirrors_node_os_tmpdir_on_posix() {
        // The sentinel-dir resolver must stay byte-parity with the JS `tmpdirNoOs()`
        // (preload-common.cjs): if the two ends disagree, the child can't find the
        // sentinel nub wrote and the compile cache silently never enables. POSIX order
        // is TMPDIR→TMP→TEMP→/tmp, trailing-slash stripped. Driven through the
        // injectable resolver so the test never mutates (parallel-safe) process env.
        let resolve = |pairs: &[(&str, &str)]| -> PathBuf {
            let map: std::collections::HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            // Mirror the live resolver: empty values are treated as unset.
            compile_cache_tmpdir_from(|k| map.get(k).cloned().filter(|s| !s.is_empty()))
        };

        // TMPDIR set → used verbatim.
        assert_eq!(
            resolve(&[("TMPDIR", "/custom/tmp")]),
            PathBuf::from("/custom/tmp")
        );
        // All unset → /tmp fallback (the case the clean `env -i` corpus harness hits).
        assert_eq!(resolve(&[]), PathBuf::from("/tmp"));
        // Trailing slash stripped (so the sentinel path doesn't double the separator).
        assert_eq!(
            resolve(&[("TMPDIR", "/custom/tmp/")]),
            PathBuf::from("/custom/tmp")
        );
        // TMP fallback when TMPDIR is unset.
        assert_eq!(resolve(&[("TMP", "/from/tmp")]), PathBuf::from("/from/tmp"));
        // TEMP fallback when TMPDIR and TMP are both unset (lowest POSIX priority).
        assert_eq!(
            resolve(&[("TEMP", "/from/temp")]),
            PathBuf::from("/from/temp")
        );
        // Priority: TMPDIR wins over TMP/TEMP when several are set.
        assert_eq!(
            resolve(&[("TMPDIR", "/win"), ("TMP", "/lose"), ("TEMP", "/lose")]),
            PathBuf::from("/win"),
        );
        // An empty TMPDIR is treated as unset → falls through to TMP.
        assert_eq!(
            resolve(&[("TMPDIR", ""), ("TMP", "/from/tmp")]),
            PathBuf::from("/from/tmp")
        );
    }

    #[test]
    #[cfg(windows)]
    fn compile_cache_tmpdir_mirrors_node_os_tmpdir_on_windows() {
        // Win32 order is TEMP→TMP→(SystemRoot|windir)\temp, trailing-backslash stripped
        // except after a drive root (`C:\`). Byte-parity with the JS `tmpdirNoOs()`
        // Win32 branch. Injectable resolver → no process-env mutation.
        let resolve = |pairs: &[(&str, &str)]| -> PathBuf {
            let map: std::collections::HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            compile_cache_tmpdir_from(|k| map.get(k).cloned().filter(|s| !s.is_empty()))
        };

        // TEMP wins (highest Win32 priority).
        assert_eq!(
            resolve(&[
                ("TEMP", "C:\\Users\\me\\AppData\\Local\\Temp"),
                ("TMP", "C:\\lose")
            ]),
            PathBuf::from("C:\\Users\\me\\AppData\\Local\\Temp"),
        );
        // TMP fallback when TEMP is unset.
        assert_eq!(
            resolve(&[("TMP", "C:\\from\\tmp")]),
            PathBuf::from("C:\\from\\tmp")
        );
        // Neither TEMP nor TMP → SystemRoot\temp.
        assert_eq!(
            resolve(&[("SystemRoot", "C:\\Windows")]),
            PathBuf::from("C:\\Windows\\temp"),
        );
        // windir is the SystemRoot fallback.
        assert_eq!(
            resolve(&[("windir", "D:\\WinDir")]),
            PathBuf::from("D:\\WinDir\\temp"),
        );
        // Trailing backslash stripped, but a bare drive root `C:\` is preserved.
        assert_eq!(
            resolve(&[("TEMP", "C:\\Temp\\")]),
            PathBuf::from("C:\\Temp")
        );
        assert_eq!(resolve(&[("TEMP", "C:\\")]), PathBuf::from("C:\\"));
    }

    #[test]
    fn path_shim_setup_and_cleanup() {
        let nub_bin = env::current_exe().unwrap();
        // Race 8 concurrent setups first — the workspace runner calls this from
        // its worker threads, and publication must be atomic (every call
        // succeeds and agrees on the dir; no loser errors with EEXIST, no
        // half-written shim). Then assert on the published result below.
        let dirs: Vec<_> = std::thread::scope(|s| {
            (0..8)
                .map(|_| s.spawn(|| setup_path_shim(&nub_bin).unwrap()))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect()
        });
        assert!(
            dirs.windows(2).all(|w| w[0] == w[1]),
            "all concurrent setups must agree on one shim dir: {dirs:?}"
        );
        let shim_dir = setup_path_shim(&nub_bin).unwrap();
        let dir = PathBuf::from(shim_dir.as_str());
        assert!(
            !fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().contains("staging")),
            "lost-race staging temps must be cleaned up"
        );

        // The shim entry is platform-specific: a `node` symlink on Unix, a
        // `node.exe` hardlink/copy on Windows (A-WIN2). Check the right one — and
        // that it actually links/points to the binary we passed.
        #[cfg(unix)]
        {
            let node_shim = dir.join("node");
            assert!(
                node_shim.symlink_metadata().is_ok(),
                "unix: node symlink created"
            );
            assert_eq!(
                fs::read_link(&node_shim).unwrap(),
                nub_bin,
                "unix: node symlinks to the nub binary"
            );
        }
        #[cfg(windows)]
        {
            let node_shim = dir.join("node.exe");
            assert!(
                node_shim.is_file(),
                "windows: node.exe (hardlink/copy) created"
            );
        }

        cleanup_shim();
        assert!(!dir.exists());
    }

    #[test]
    fn permission_flag_classifier_is_exact_not_a_prefix_match() {
        // Real permission flags engage Node's sandbox (and need --allow-addons).
        assert!(is_permission_flag("--permission"));
        assert!(is_permission_flag("--allow-addons"));
        // Value-taking permission flags appear as `--flag=value`; match up to `=`.
        assert!(is_permission_flag("--allow-fs-read=/tmp"));
        assert!(is_permission_flag("--allow-net=localhost"));
        // --allow-ffi is a real permission flag on the Node versions that have it.
        assert!(is_permission_flag("--allow-ffi"));

        // The bug R5 fixes: a V8 flag that shares the --allow- prefix but is NOT a
        // permission flag. Stock node runs `--allow-natives-syntax x.js`; the old
        // prefix match aborted it as "--permission requires --allow-addons".
        assert!(!is_permission_flag("--allow-natives-syntax"));
        // Plain user args and other --allow-*-looking-but-unknown tokens don't trip it.
        assert!(!is_permission_flag("--enable-source-maps"));
        assert!(!is_permission_flag("script.js"));
    }

    #[test]
    fn coverage_exclude_targets_absolute_runtime_dir_only_when_coverage_active() {
        let preload = "/opt/nub/runtime/preload.mjs";

        // No coverage flag anywhere → no exclude injected.
        assert!(coverage_exclude_glob(&[], None, Some(preload)).is_none());

        // Coverage via argv → exclude keyed to the ABSOLUTE runtime dir (the
        // preload's parent), with a trailing `/**` — not a broad `**/runtime/**`.
        let argv = vec![
            "--test".to_string(),
            "--experimental-test-coverage".to_string(),
        ];
        assert_eq!(
            coverage_exclude_glob(&argv, None, Some(preload)).as_deref(),
            Some("--test-coverage-exclude=/opt/nub/runtime/**"),
        );

        // Coverage via NODE_OPTIONS is detected the same way.
        assert_eq!(
            coverage_exclude_glob(&[], Some("--experimental-test-coverage"), Some(preload))
                .as_deref(),
            Some("--test-coverage-exclude=/opt/nub/runtime/**"),
        );

        // Coverage active but no resolvable preload → nothing to exclude.
        assert!(coverage_exclude_glob(&argv, None, None).is_none());
    }

    #[test]
    fn compile_cache_coverage_gate_fires_on_every_coverage_channel() {
        // The compile-cache/coverage gate (Fix 3): nub must set up NO compile cache
        // when this run is collecting V8 coverage, because a warm cache collapses
        // V8's per-branch ranges. Coverage engages through three channels — gate on
        // all of them.
        let cov_argv = vec![
            "--test".to_string(),
            "--experimental-test-coverage".to_string(),
        ];
        let plain_argv = vec!["app.js".to_string()];

        // (1) Coverage via argv.
        assert!(coverage_active_for_cache(&cov_argv, None, None));
        // (2) Coverage via NODE_OPTIONS.
        assert!(coverage_active_for_cache(
            &plain_argv,
            Some("--experimental-test-coverage"),
            None
        ));
        // (3) Coverage via NODE_V8_COVERAGE env (no flag anywhere) — the channel
        //     coverage_active (R9 exclude-glob) does NOT cover, but the cache gate
        //     must, since `NODE_V8_COVERAGE=<dir> node app.js` collects coverage
        //     with no flag.
        assert!(coverage_active_for_cache(
            &plain_argv,
            None,
            Some("/tmp/cov")
        ));

        // No coverage signal on any channel → gate stays OFF (cache enabled). An
        // EMPTY NODE_V8_COVERAGE is not coverage (Node treats empty as disabled),
        // and a user-set NODE_COMPILE_CACHE is intentionally not consulted here —
        // its preservation is the caller's concern, not this gate's.
        assert!(!coverage_active_for_cache(&plain_argv, None, None));
        assert!(!coverage_active_for_cache(
            &plain_argv,
            Some("--enable-source-maps"),
            None
        ));
        assert!(!coverage_active_for_cache(&plain_argv, None, Some("")));
    }

    #[test]
    fn reentrancy_matches_full_preload_path_not_filename_substring() {
        let ours = "/opt/nub/runtime/preload.mjs";

        // The A26 bug: a user's OWN --import of a file merely named preload.mjs
        // must NOT register as ours (the old substring check did, and wrongly
        // disabled augmentation).
        assert!(
            !is_reentrant_in(Some("--import=file:///home/me/app/preload.mjs"), Some(ours),),
            "a user's unrelated preload.mjs must not be mistaken for nub's"
        );

        // NODE_OPTIONS carrying our actual preload path IS re-entrant (a parent
        // nub injected it), even alongside other flags and a user import.
        assert!(
            is_reentrant_in(
                Some(&format!(
                    "--experimental-vm-modules --import=file://{ours} --import=file:///u/preload.mjs"
                )),
                Some(ours),
            ),
            "our own preload path in NODE_OPTIONS means a parent nub already augmented"
        );

        // Degenerate inputs are never re-entrant.
        assert!(!is_reentrant_in(None, Some(ours)), "no NODE_OPTIONS set");
        assert!(
            !is_reentrant_in(Some("--import=file:///x/preload.mjs"), None),
            "no preload resolved"
        );
        assert!(!is_reentrant_in(Some(""), Some(ours)), "empty NODE_OPTIONS");
    }

    #[test]
    fn preload_injection_is_require_cjs_on_fast_tier_import_mjs_on_compat() {
        let mjs = "/opt/nub/runtime/preload.mjs";

        // Fast tier (>= 22.15): `--require` the sibling CJS preload by raw PATH
        // (require does not accept a file:// URL). This is the channel that keeps
        // Node's synchronous CJS entry path (the R1 fix).
        let fast = preload_injection_for(mjs, &NodeVersion::new(22, 15, 0), false);
        assert_eq!(fast.flag, "--require");
        assert_eq!(fast.value, "/opt/nub/runtime/preload.cjs");
        assert_eq!(
            fast.node_options_token(),
            "--require=/opt/nub/runtime/preload.cjs"
        );

        // A clearly-fast version too (24.x).
        let fast24 = preload_injection_for(mjs, &NodeVersion::new(24, 0, 0), false);
        assert_eq!(fast24.flag, "--require");
        assert_eq!(fast24.value, "/opt/nub/runtime/preload.cjs");

        // Compat tier (< 22.15): `--import` the ESM preload by file:// URL — the
        // async path stays unchanged.
        let compat = preload_injection_for(mjs, &NodeVersion::new(20, 11, 0), false);
        assert_eq!(compat.flag, "--import");
        assert_eq!(compat.value, "file:///opt/nub/runtime/preload.mjs");
        assert_eq!(
            compat.node_options_token(),
            "--import=file:///opt/nub/runtime/preload.mjs"
        );

        // The 22.14.x boundary stays on the compat (import) channel.
        let boundary = preload_injection_for(mjs, &NodeVersion::new(22, 14, 99), false);
        assert_eq!(boundary.flag, "--import");
    }

    #[test]
    fn file_url_unix_is_file_plus_path() {
        assert_eq!(
            to_file_url("/opt/nub/runtime/preload.mjs", false),
            "file:///opt/nub/runtime/preload.mjs"
        );
    }

    #[test]
    fn file_url_windows_drive_and_verbatim() {
        // Plain drive path.
        assert_eq!(
            to_file_url(r"C:\npm\prefix\runtime\preload.mjs", true),
            "file:///C:/npm/prefix/runtime/preload.mjs"
        );
        // The exact path from the 0.0.9 windows test-install failure: a canonicalized
        // `\\?\` verbatim path. A naive `file://` + path produced the malformed
        // `file:////?\C:\...` that Node rejected (ERR_INVALID_FILE_URL_PATH).
        assert_eq!(
            to_file_url(
                r"\\?\C:\npm\prefix\node_modules\@nubjs\nub\node_modules\@nubjs\nub-win32-x64\runtime\preload.mjs",
                true
            ),
            "file:///C:/npm/prefix/node_modules/@nubjs/nub/node_modules/@nubjs/nub-win32-x64/runtime/preload.mjs"
        );
        // UNC verbatim path -> file://server/share/...
        assert_eq!(
            to_file_url(r"\\?\UNC\server\share\runtime\preload.mjs", true),
            "file://server/share/runtime/preload.mjs"
        );
    }

    #[test]
    fn strip_verbatim_removes_windows_prefixes_only() {
        assert_eq!(strip_verbatim(r"\\?\C:\a\b", true), r"C:\a\b");
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\sh", true), r"\\srv\sh");
        assert_eq!(strip_verbatim(r"C:\a\b", true), r"C:\a\b"); // no prefix: unchanged
        // Non-Windows host never strips (a unix path could legitimately start oddly).
        assert_eq!(strip_verbatim(r"\\?\C:\a", false), r"\\?\C:\a");
    }

    #[test]
    fn reentrancy_holds_through_url_keying_on_windows() {
        // The fix's invariant: the parent injects `--import=<url>` into NODE_OPTIONS,
        // and the child detects re-entrancy by finding that URL — for the SAME
        // canonicalized preload path on both sides. Proven for the Windows verbatim
        // path on any host via the `windows` param.
        let raw = r"\\?\C:\app\runtime\preload.mjs";
        let url = to_file_url(raw, true);
        let injected = format!("--experimental-vm-modules --import={url}");
        assert!(
            is_reentrant_in(Some(&injected), Some(&url)),
            "child must detect the parent-injected url in NODE_OPTIONS"
        );
        // And an unrelated user preload.mjs still must not false-positive.
        assert!(
            !is_reentrant_in(Some("--import=file:///C:/me/app/preload.mjs"), Some(&url)),
            "a different preload path must not register as ours"
        );
    }

    #[test]
    fn vendored_node_path_present_only_for_installed_package() {
        let tmp = env::temp_dir().join(format!("nub-a30-{}", std::process::id()));
        let runtime = tmp.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let preload = runtime.join("preload.mjs");
        fs::write(&preload, "").unwrap();
        let preload_str = preload.to_str().unwrap();

        // Dev: runtime/ has no node_modules → None (CJS requires resolve by
        // walking up to the repo's node_modules; no NODE_PATH needed).
        assert!(
            vendored_node_path(Some(preload_str)).is_none(),
            "no node_modules → None"
        );

        // Installed package: runtime/node_modules exists → NODE_PATH leads with it.
        let vendored = runtime.join("node_modules");
        fs::create_dir_all(&vendored).unwrap();
        let np = vendored_node_path(Some(preload_str)).expect("node_modules present → Some");
        assert!(
            np.to_string_lossy().starts_with(vendored.to_str().unwrap()),
            "NODE_PATH leads with the vendored node_modules, got {np:?}"
        );

        assert!(vendored_node_path(None).is_none(), "no preload → None");
        let _ = fs::remove_dir_all(&tmp);
    }
}
