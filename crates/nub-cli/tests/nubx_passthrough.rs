//! nubx post-target `--` passthrough across every execution tier — the durable,
//! oracle-grounded contract that `nubx <subject> [args]` keeps everything after
//! the resolved subject byte-for-byte, including the post-target `--` (Option A,
//! decided 2026-06-28). This is the nubx companion to `target_passthrough.rs`:
//! that file locks the `nub run`/`exec`/`watch`/`<file>` runners; this one locks
//! the unified `nubx` runner, whose resolver (`nubx_resolve`) routes a subject to
//! one of those runners by tier.
//!
//! Why each tier already conforms (verified here, not assumed): the resolver's
//! scan locates the SUBJECT and returns at it, so a POST-target `--` is never
//! seen by the resolver — it rides the verbatim tail into the tier's runner:
//!   - file   → the file runner (node-identical; keeps `--`)
//!   - script → re-dispatch to `nub run`  → `split_subcommand_argv` (Option A)
//!   - bin    → re-dispatch to `nubx`/exec → `split_subcommand_argv` (Option A)
//!
//! The registry tier is the bin path after a consent-gated network fetch, so it
//! shares the bin tier's passthrough; it needs the network + a consent ledger and
//! is not exercised offline.
//!
//! Golden values match the reference tools (offline, deterministic): node 26 for
//! the file tier, pnpm 10 `run` for the script tier, pnpm 10 `exec` for the bin
//! tier — the same oracles `target_passthrough.rs` documents. Each case ALSO
//! asserts nubx equals the corresponding already-oracle-grounded `nub` path
//! (`nub <file>` / `nub run` / `nub exec`), tying the two harnesses together.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    // `nub` on unix, `nub.exe` on Windows. `Command::new` auto-appends `.exe` on
    // Windows so the bare name spawns fine, but the `std::fs::copy(nub → nubx)`
    // below does NOT — it needs the real filename or the source doesn't exist and
    // the copy panics. EXE_SUFFIX is "" off Windows.
    path.push(format!("nub{}", std::env::consts::EXE_SUFFIX));
    path
}

/// A `nubx`-named handle to the nub binary. nubx is argv0 dispatch (the runner is
/// selected from argv[0]'s file stem), so exercising the real `run_nubx` path —
/// and thus the resolver — requires invoking the binary AS `nubx`, not `nub exec`
/// (which bypasses the resolver). Symlink on unix, copy on Windows (no symlink
/// privilege), created once per test process. Mirrors the meta test in
/// `integration.rs`.
fn nubx_binary() -> &'static Path {
    static NUBX: OnceLock<PathBuf> = OnceLock::new();
    NUBX.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nub-nubx-passthrough-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let nubx = dir.join(if cfg!(windows) { "nubx.exe" } else { "nubx" });
        if !nubx.exists() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(nub_binary(), &nubx).expect("symlink nub → nubx");
            #[cfg(not(unix))]
            std::fs::copy(nub_binary(), &nubx).expect("copy nub → nubx");
        }
        nubx
    })
}

fn fixture_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest).join("../../tests/fixtures/target-passthrough")
}

fn svec(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn parse_argv(stdout: &str, stderr: &str) -> Vec<String> {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("ARGV:"))
        .unwrap_or_else(|| {
            panic!("fixture never emitted an ARGV line\nstdout: {stdout:?}\nstderr: {stderr:?}")
        });
    serde_json::from_str(line).expect("fixture must emit a JSON argv array")
}

/// Run a program in the fixture dir and return the argv the executed target saw.
fn argv_of(program: &Path, args: &[&str]) -> Vec<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(fixture_dir())
        .output()
        .unwrap_or_else(|e| panic!("spawn {program:?}: {e}"));
    parse_argv(
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

/// `nubx <args>` → the argv the resolved tier's target received.
fn nubx_argv(args: &[&str]) -> Vec<String> {
    argv_of(nubx_binary(), args)
}

/// `nub <args>` → the argv via the corresponding already-oracle-grounded runner.
fn nub_argv(args: &[&str]) -> Vec<String> {
    argv_of(&nub_binary(), args)
}

/// FILE tier: `nubx <file>` runs the file via the node-identical file runner. The
/// post-target `--` is kept; flags pass through without it; every `--` is literal.
/// Oracle: node 26. Cross-checked against `nub <file>` (same runner).
#[test]
fn nubx_file_tier_keeps_dashdash_like_node() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["echo-argv.js", "--", "--foo", "bar"],
            &["--", "--foo", "bar"],
        ),
        (&["echo-argv.js", "--foo", "bar"], &["--foo", "bar"]), // no `--`: flags still pass
        (
            &["echo-argv.js", "--", "a", "--", "b"],
            &["--", "a", "--", "b"],
        ), // repeated `--`
    ];
    for (input, want) in cases {
        assert_eq!(
            nubx_argv(input),
            svec(want),
            "nubx file tier argv for `nubx {}`",
            input.join(" ")
        );
        assert_eq!(
            nubx_argv(input),
            nub_argv(input),
            "nubx file tier must equal `nub {}`",
            input.join(" ")
        );
    }
}

/// SCRIPT tier: `nubx <script>` re-dispatches to `nub run`, inheriting Option A.
/// Oracle: pnpm 10 `run`. Cross-checked against `nub run <script>`.
#[test]
fn nubx_script_tier_keeps_dashdash_like_pnpm_run() {
    // (nubx args, the equivalent `nub run` args, expected child argv)
    let cases: &[(&[&str], &[&str], &[&str])] = &[
        (
            &["echo", "--", "--foo", "bar"],
            &["run", "echo", "--", "--foo", "bar"],
            &["--", "--foo", "bar"],
        ),
        (
            &["echo", "--foo", "bar"],
            &["run", "echo", "--foo", "bar"],
            &["--foo", "bar"],
        ),
        (
            &["echo", "--", "a", "--", "b"],
            &["run", "echo", "--", "a", "--", "b"],
            &["--", "a", "--", "b"],
        ),
    ];
    for (nubx_in, run_in, want) in cases {
        assert_eq!(
            nubx_argv(nubx_in),
            svec(want),
            "nubx script tier argv for `nubx {}`",
            nubx_in.join(" ")
        );
        assert_eq!(
            nubx_argv(nubx_in),
            nub_argv(run_in),
            "nubx script tier must equal `nub {}`",
            run_in.join(" ")
        );
    }
}

/// BIN tier: `nubx <bin>` re-dispatches to the `nubx`/exec clap path, inheriting
/// Option A. Oracle: pnpm 10 `exec` (which keeps `--`; the no-exec-bit committed
/// fixture bin can't be run by pnpm directly, so the oracle is carried through
/// `nub exec`, itself grounded against pnpm 10 in `target_passthrough.rs`).
#[test]
fn nubx_bin_tier_keeps_dashdash_like_pnpm_exec() {
    let cases: &[(&[&str], &[&str], &[&str])] = &[
        (
            &["echo-argv", "--", "--foo", "bar"],
            &["exec", "echo-argv", "--", "--foo", "bar"],
            &["--", "--foo", "bar"],
        ),
        (
            &["echo-argv", "--foo", "bar"],
            &["exec", "echo-argv", "--foo", "bar"],
            &["--foo", "bar"],
        ),
        (
            &["echo-argv", "--", "a", "--", "b"],
            &["exec", "echo-argv", "--", "a", "--", "b"],
            &["--", "a", "--", "b"],
        ),
    ];
    for (nubx_in, exec_in, want) in cases {
        assert_eq!(
            nubx_argv(nubx_in),
            svec(want),
            "nubx bin tier argv for `nubx {}`",
            nubx_in.join(" ")
        );
        assert_eq!(
            nubx_argv(nubx_in),
            nub_argv(exec_in),
            "nubx bin tier must equal `nub {}`",
            exec_in.join(" ")
        );
    }
}

/// The subject boundary is resolved before the passthrough begins: a Node
/// value-flag (`--title`) whose VALUE looks like the file subject binds the
/// value, so the real subject is the token after it and only its trailing args
/// forward — the resolver must not mistake the value for the subject (or strip
/// the tail wrong). nubx-resolver-specific; the runner harness can't reach it.
/// `--title` (vs `--env-file`) is chosen because it is floor-compatible: it takes
/// an arbitrary string and exists on every supported Node, including the 18.19
/// compat-tier floor — `--env-file` is a Node-20.6+ flag node 18.19 rejects with
/// "bad option" before the fixture can run.
#[test]
fn nubx_resolver_value_flag_value_not_mistaken_for_subject() {
    assert_eq!(
        nubx_argv(&["--title", "echo-argv.js", "echo-argv.js", "x"]),
        svec(&["x"]),
        "a value-flag value that looks like the subject must bind to the flag"
    );
}
