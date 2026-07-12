//! The bundled cross-platform POSIX-subset script shell, backed by
//! `deno_task_shell`. This is the DEFAULT engine for `nub run` package scripts
//! (opt out with `.npmrc` `shell-emulator=false`, or override to a real shell
//! binary with `--script-shell`). It gives scripts the same POSIX behavior on
//! Windows as on Unix without requiring a system `sh` — `rm -rf`, `&&`, `$VAR`,
//! `cp`/`mkdir`, and friends run through Rust builtins or a `PATH`/`which`
//! lookup that spawns the real binary.
//!
//! The load-bearing correctness property: deno spawns unresolved commands with
//! `.env_clear().envs(state.env_vars())`, so the env map handed to `execute` is
//! the COMPLETE environment a `node`/`tsc` launched inside a script sees. It
//! must therefore carry the full parent environment PLUS nub's augmentation
//! overrides (PATH shim, `NODE_OPTIONS`, …) — exactly what the native `sh -c`
//! child inherits — or transpilation would silently not reach script children.
//! `build_env_map` seeds from the parent env and layers the same override pairs
//! the native `Command` path applies (assembled once in `cli::build_script_command`).
//!
//! deno's executor is `!Send` (it holds `Rc`s and uses `spawn_local` for `&`
//! background jobs), so it MUST run on a current-thread tokio runtime driven
//! through a `LocalSet` — a plain `block_on` panics the moment a script uses a
//! trailing `&`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use deno_task_shell::{
    KillSignal, ShellPipeReader, ShellState, SignalKind, execute, execute_with_pipes, parser,
};

use crate::cli::DrainPolicy;

/// Appended to every parse error. The emulator is default-on (a breaking change
/// vs a native shell), so a user hitting a construct outside the subset —
/// arithmetic `$((…))`, unbalanced quotes — must be pointed at the escape hatch,
/// not left with a bare parse error. (Control-flow keywords like `for`/`if` do
/// NOT parse-error here — deno treats them as command words → `for: command not
/// found`, exit 127 — so they can't be intercepted; the docs cover that case.)
const EMULATOR_OPT_OUT_HINT: &str = "\n(nub runs scripts through a POSIX-subset shell by default; set shell-emulator=false in .npmrc to use your platform's native shell)";

/// Parse a script body, tagging any failure with the opt-out hint.
fn parse_body(body: &str) -> Result<parser::SequentialList> {
    parser::parse(body)
        .map_err(|e| anyhow::anyhow!("shell parse error: {e}{EMULATOR_OPT_OUT_HINT}"))
}

/// deno's `ShellState::new` asserts the cwd is absolute, so a relative project
/// root would hard-abort under `panic = "abort"`. The native `Command` path
/// tolerates a relative root via `current_dir`; absolutize here (lexically, or
/// joined onto the process cwd) so the emulator matches instead of panicking,
/// surfacing a clean error if the cwd can't be resolved.
fn absolute_cwd(cwd: PathBuf) -> Result<PathBuf> {
    if cwd.is_absolute() {
        Ok(cwd)
    } else {
        std::path::absolute(&cwd)
            .with_context(|| format!("resolve script working directory {}", cwd.display()))
    }
}

/// Run a script body through the emulator with inherited stdio (single-package
/// `nub run`). Returns the shell's exit code.
pub(crate) fn run_inherit(body: &str, env: Vec<(OsString, OsString)>, cwd: PathBuf) -> Result<i32> {
    let list = parse_body(body)?;
    let cwd = absolute_cwd(cwd)?;
    let env_map = build_env_map(env);
    run_on_local_runtime(async move {
        let kill = KillSignal::default();
        spawn_signal_forwarder(kill.clone());
        execute(list, env_map, cwd, Default::default(), kill).await
    })
}

/// Run a script body through the emulator with piped stdout/stderr so each line
/// can be prefixed/collected (workspace `-r`, `--stream`, ndjson, aggregate).
/// The two [`DrainPolicy`] instances format+emit each line exactly as the native
/// piped path does. Returns `(exit_code, stdout_lines, stderr_lines)`.
pub(crate) fn run_prefixed(
    body: &str,
    env: Vec<(OsString, OsString)>,
    cwd: PathBuf,
    out_policy: DrainPolicy,
    err_policy: DrainPolicy,
) -> Result<(i32, Vec<String>, Vec<String>)> {
    let list = parse_body(body)?;
    let cwd = absolute_cwd(cwd)?;
    let env_map = build_env_map(env);

    let (out_reader, out_writer) = deno_task_shell::pipe();
    let (err_reader, err_writer) = deno_task_shell::pipe();

    // Drain both pipes on dedicated threads: `block_on` occupies THIS thread
    // driving `execute`, so if nothing reads the pipes concurrently deno blocks
    // forever once a pipe buffer fills. `Builder::spawn` (not `thread::spawn`)
    // so thread-create pressure surfaces as an error, never a `panic = "abort"`
    // process abort. The moved writers are the pipes' only write ends; `execute`
    // drops them on completion → the readers hit EOF → these threads finish.
    let out_handle = std::thread::Builder::new()
        .name("nub-emul-out".into())
        .spawn(move || out_policy.run(into_pipe_reader(out_reader)))
        .context("spawn shell-emulator stdout drain thread")?;
    let err_handle = std::thread::Builder::new()
        .name("nub-emul-err".into())
        .spawn(move || err_policy.run(into_pipe_reader(err_reader)))
        .context("spawn shell-emulator stderr drain thread")?;

    let code = run_on_local_runtime(async move {
        let kill = KillSignal::default();
        spawn_signal_forwarder(kill.clone());
        let state = ShellState::new(env_map, cwd, Default::default(), kill);
        execute_with_pipes(
            list,
            state,
            ShellPipeReader::stdin(),
            out_writer,
            err_writer,
        )
        .await
    })?;

    let out_lines = out_handle
        .join()
        .map_err(|_| anyhow::anyhow!("shell-emulator stdout drain thread panicked"))?;
    let err_lines = err_handle
        .join()
        .map_err(|_| anyhow::anyhow!("shell-emulator stderr drain thread panicked"))?;
    Ok((code, out_lines, err_lines))
}

/// Seed the map from the parent environment, then layer nub's augmentation
/// overrides on top (last-wins, the same order the native `Command` path applies
/// them). See the module doc for why the full parent env must be present.
fn build_env_map(overrides: Vec<(OsString, OsString)>) -> HashMap<OsString, OsString> {
    let mut map: HashMap<OsString, OsString> = std::env::vars_os().collect();
    for (k, v) in overrides {
        map.insert(k, v);
    }
    map
}

/// `pipe()` always yields the `OsPipe` variant; extract the inner reader (which
/// implements `Read + Send`) so a `DrainPolicy` can consume it on a thread.
fn into_pipe_reader(reader: ShellPipeReader) -> std::io::PipeReader {
    match reader {
        ShellPipeReader::OsPipe(r) => r,
        ShellPipeReader::StdFile(_) => {
            unreachable!("deno_task_shell::pipe() always returns an OsPipe reader")
        }
    }
}

/// Build a current-thread runtime + `LocalSet` and drive `fut` to completion.
/// deno's executor is `!Send`; a `LocalSet` is what lets a trailing-`&` job's
/// `spawn_local` run instead of panicking. Called from sync code (never inside
/// another runtime), so `block_on` is safe.
fn run_on_local_runtime<F: std::future::Future>(fut: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for shell emulator")?;
    let local = tokio::task::LocalSet::new();
    Ok(local.block_on(&rt, fut))
}

/// Forward terminating OS signals to deno's `KillSignal` so `docker stop` /
/// systemd / `kill` reach the emulated script's children — without it a child
/// `node` is orphaned on SIGTERM (deno kills tracked children on abort). Runs as
/// a `spawn_local` task on the same thread as the executor (both `!Send`), and
/// is cancelled when the `LocalSet` is dropped after the script exits.
///
/// SIGINT is forwarded ONLY when stdin is not a TTY: on a terminal the kernel
/// already delivers Ctrl-C's SIGINT to the whole foreground group (deno's child
/// included), so forwarding it too would deliver it twice (the double-`SIGINT`
/// class of bug, cf. issue #26). SIGTERM/SIGHUP/SIGQUIT are group-independent
/// and always forwarded.
fn spawn_signal_forwarder(kill: KillSignal) {
    tokio::task::spawn_local(async move { forward_signals(kill).await });
}

#[cfg(unix)]
async fn forward_signals(kill: KillSignal) {
    use std::io::IsTerminal;
    use tokio::signal::unix::{Signal, SignalKind as Unix, signal};

    async fn next(sig: &mut Option<Signal>) {
        match sig {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    let mut term = signal(Unix::terminate()).ok();
    let mut hup = signal(Unix::hangup()).ok();
    let mut quit = signal(Unix::quit()).ok();
    let mut int = if std::io::stdin().is_terminal() {
        None
    } else {
        signal(Unix::interrupt()).ok()
    };

    loop {
        tokio::select! {
            _ = next(&mut term) => kill.send(SignalKind::SIGTERM),
            // SIGHUP maps to `Other(1)`, which deno delivers to the in-flight
            // child (OS `kill(pid, SIGHUP)`) but does NOT treat as an abort, so a
            // `;`-separated command after it could still run. Acceptable: SIGHUP
            // reaching the running workload is the load-bearing behavior.
            _ = next(&mut hup) => kill.send(SignalKind::Other(1)),
            _ = next(&mut quit) => kill.send(SignalKind::SIGQUIT),
            _ = next(&mut int) => kill.send(SignalKind::SIGINT),
        }
    }
}

#[cfg(not(unix))]
async fn forward_signals(kill: KillSignal) {
    // Windows: deno associates children with a Job Object that kills them when
    // nub exits, so orphaning is already handled; forward Ctrl-C so an
    // interactive interrupt still aborts a running child promptly.
    while tokio::signal::ctrl_c().await.is_ok() {
        kill.send(SignalKind::SIGINT);
    }
}
