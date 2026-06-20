//! Differential CLI-grammar acceptance test for the install-family surface.
//!
//! WHY THIS EXISTS (the #29 / P0 blind spot). nub routes a verb through one of
//! two parsers: nub's own hand-written clap structs (`install`/`i`/`ci`/
//! `upgrade`) and the engine verbs (`add`/`remove`/`update`/… — parsed with
//! aube's `Args` types). Every historical flag/positional compat bug has lived
//! at the seam between those two parsers, and nothing crossed it: the native
//! install/ci structs had ZERO parse coverage, and the engine-verb parse test
//! only asserted what aube HAS, never what pnpm/npm document. `nub install -g
//! <pkg>` (#29) and `nub install <pkg>` (its strictly-more-common twin) both
//! shipped rejecting at clap because of exactly that gap.
//!
//! WHAT THIS GUARDS. A hand-curated, deliberately-WIDE table of the npm- AND
//! pnpm-DOCUMENTED flag / alias / positional forms users actually type for each
//! install-family verb. Every form is grounded in the incumbent's own `--help`
//! output (NOT "the incumbent didn't error" — npm and pnpm are lenient and
//! silently accept unknown flags, a false-negative trap), so a row asserts a
//! form a real PM documents as supported. For each row we assert nub's parser
//! ACCEPTS the grammar: spawn `nub <form> --help` and FAIL iff clap emitted
//! `unexpected argument` / `unrecognized …` (its parse-reject markers). The
//! table spans BOTH parsers in one place, so the routing seam is always crossed.
//!
//! Appending `--help` makes each spawn a pure parse-then-print — clap intercepts
//! `--help` once the argv parses, so no install runs and no network is touched.
//! A runtime failure (no lockfile, bin-not-found → exit 127) is NOT a parse
//! reject and does not fail a row; only the clap reject markers do.
//!
//! KNOWN-STILL-BROKEN rows (forms an incumbent documents but nub rejects today)
//! live in `#[ignore]`d tests below, each naming its follow-up, so the gap is
//! documented-not-silently-missing.

use std::path::PathBuf;
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push(format!("nub{}", std::env::consts::EXE_SUFFIX));
    path
}

/// Spawn `nub <args> --help` in an isolated, network-free-ish env and return
/// whether clap REJECTED the grammar (its `unexpected argument` /
/// `unrecognized` markers). A non-reject (parse OK → help printed, or a runtime
/// error past the parser) returns `false`.
fn clap_rejected(args: &[&str]) -> (bool, String) {
    let tmp = std::env::temp_dir().join(format!(
        "nub-grammar-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '='], "-")
    ));
    let _ = std::fs::create_dir_all(&tmp);
    // A bare manifest so the verbs that peek at package.json don't bail before
    // the parser is even exercised.
    let _ = std::fs::write(tmp.join("package.json"), r#"{"name":"t","version":"1.0.0"}"#);

    let out = Command::new(nub_binary())
        .args(args)
        .arg("--help")
        .current_dir(&tmp)
        // Isolate cache/home so no real global/cache state is touched.
        .env("HOME", &tmp)
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("PNPM_HOME", tmp.join("pnpm"))
        .output()
        .expect("spawn nub");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&tmp);
    let rejected = combined.contains("unexpected argument") || combined.contains("unrecognized");
    (rejected, combined)
}

/// Assert nub accepts every form in the table; on failure, name the form and
/// dump nub's output so the cause is obvious without rerunning.
fn assert_all_accepted(label: &str, rows: &[(&[&str], &str)]) {
    let mut failures = Vec::new();
    for (form, note) in rows {
        let (rejected, output) = clap_rejected(form);
        if rejected {
            failures.push(format!(
                "  nub {} → REJECTED (npm/pnpm document this: {note})\n    output: {}",
                form.join(" "),
                output.lines().next().unwrap_or("").trim()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{label}: {} of {} documented incumbent forms rejected by nub's parser:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// install / i — native clap struct + the install→add routing seam (A/B/C/D).
// Grounded in `npm install -h` and `pnpm install/add --help`.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn install_family_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "install/i",
        &[
            // Bare install + the native lockfile/dep/workspace knobs (pnpm).
            (&["install"], "pnpm install"),
            (&["i"], "pnpm i (alias)"),
            (&["install", "--frozen-lockfile"], "pnpm --frozen-lockfile"),
            (&["install", "--no-frozen-lockfile"], "pnpm --no-frozen-lockfile"),
            (&["install", "--prefer-frozen-lockfile"], "pnpm --prefer-frozen-lockfile"),
            (&["install", "-P"], "pnpm -P / --prod"),
            (&["install", "--prod"], "pnpm --prod"),
            (&["install", "-D"], "pnpm -D (dev only)"),
            (&["install", "--ignore-scripts"], "npm/pnpm --ignore-scripts"),
            (&["install", "--no-optional"], "pnpm --no-optional"),
            (&["install", "--offline"], "pnpm --offline"),
            (&["install", "--prefer-offline"], "pnpm --prefer-offline"),
            (&["install", "--lockfile-only"], "pnpm --lockfile-only"),
            (&["install", "-r"], "pnpm -r"),
            (&["install", "-F", "foo"], "pnpm -F <pattern>"),
            (&["install", "--filter", "foo"], "pnpm --filter <pattern>"),
            (&["install", "-C", "/tmp"], "pnpm -C <dir>"),
            // A/B/C — install <pkg> is the add-to-deps form (the P0 + its flags).
            (&["install", "express"], "npm/pnpm install <pkg> adds to deps (P0)"),
            (&["i", "lodash"], "npm/pnpm i <pkg> (P0)"),
            (&["install", "express", "lodash"], "multiple package specs"),
            (&["install", "express", "-D"], "npm/pnpm -D <pkg>"),
            (&["install", "express", "--save-dev"], "npm --save-dev"),
            (&["install", "express", "-E"], "npm -E / --save-exact"),
            (&["install", "express", "--save-exact"], "npm --save-exact"),
            (&["install", "express", "-O"], "npm -O / --save-optional"),
            (&["install", "express", "--save-optional"], "npm --save-optional"),
            (&["install", "express", "--no-save"], "npm --no-save"),
            (&["install", "express", "-S"], "npm -S / --save"),
            (&["install", "express", "--save"], "npm --save"),
            (&["install", "express", "--save-prod"], "npm --save-prod"),
            (&["install", "express", "-P"], "npm -P on an add (save to deps, default)"),
            (&["install", "express", "--omit=dev"], "npm --omit=<dev|optional|peer>"),
            (&["install", "express", "--omit", "dev"], "npm --omit <x> (space form)"),
            (&["install", "express", "-g"], "npm/pnpm install -g <pkg> (#29)"),
            (&["install", "express", "--global"], "npm/pnpm --global"),
            (&["install", "express", "-w", "foo"], "npm -w <member> selector"),
            (&["install", "express", "--workspace", "foo"], "npm --workspace <member>"),
            (&["install", "express", "--workspaces"], "npm --workspaces"),
        ],
    );
}

// D — leading global flags before an install-family verb (pnpm `-r install`).
#[test]
fn leading_global_flags_before_install_family() {
    assert_all_accepted(
        "leading-flag order",
        &[
            (&["-r", "install"], "pnpm -r install"),
            (&["-r", "i"], "pnpm -r i"),
            (&["-F", "foo", "install"], "pnpm -F <pattern> install"),
            (&["--filter", "foo", "install"], "pnpm --filter <pattern> install"),
            (&["-r", "ci"], "pnpm -r ci"),
            (&["-r", "update"], "pnpm -r update"),
            (&["-r", "dedupe"], "pnpm -r dedupe"),
            (&["-r", "add", "foo"], "pnpm -r add <pkg>"),
            (&["-r", "remove", "foo"], "pnpm -r remove <pkg>"),
            // Sanity: the original run/exec normalization still holds.
            (&["-r", "run", "build"], "pnpm -r run <script> (unchanged)"),
        ],
    );
}

// F — `nub ci` production controls (npm `ci --omit=dev`).
#[test]
fn ci_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "ci",
        &[
            (&["ci"], "npm ci"),
            (&["ci", "--ignore-scripts"], "npm/pnpm --ignore-scripts"),
            (&["ci", "--no-optional"], "pnpm --no-optional"),
            (&["ci", "-P"], "production-only (F)"),
            (&["ci", "--production"], "--production alias (F)"),
            (&["ci", "--omit=dev"], "npm ci --omit=dev (production deploy) (F)"),
            (&["ci", "--omit=optional"], "npm ci --omit=optional (F)"),
            (&["ci", "--omit=peer"], "npm ci --omit=peer (no-op, accepted) (F)"),
            (&["ci", "-r"], "pnpm -r ci"),
            (&["ci", "-F", "foo"], "pnpm -F <pattern>"),
        ],
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Engine verbs — the other side of the routing seam. Each parses with aube's
// own `Args` + nub's `EngineGlobals`. Grounded in `pnpm <verb> --help` (the CLI
// surface nub claims parity with) and `npm <verb> -h`.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn engine_add_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "add",
        &[
            (&["add", "foo"], "pnpm add <pkg>"),
            (&["a", "foo"], "pnpm a (alias)"),
            (&["add", "foo", "bar"], "multiple specs"),
            (&["add", "foo", "-D"], "pnpm -D / --save-dev"),
            (&["add", "foo", "--save-dev"], "pnpm --save-dev"),
            (&["add", "foo", "-E"], "pnpm -E / --save-exact"),
            (&["add", "foo", "--save-exact"], "pnpm --save-exact"),
            (&["add", "foo", "-O"], "pnpm -O / --save-optional"),
            (&["add", "foo", "--save-optional"], "pnpm --save-optional"),
            (&["add", "foo", "--save-peer"], "pnpm --save-peer"),
            (&["add", "foo", "--no-save"], "pnpm --no-save"),
            (&["add", "foo", "-g"], "pnpm -g / --global"),
            (&["add", "foo", "--global"], "pnpm --global"),
            (&["add", "foo", "-w"], "pnpm -w (add to workspace root)"),
            (&["add", "foo", "-r"], "pnpm -r"),
            (&["add", "foo", "-F", "bar"], "pnpm -F <pattern>"),
            (&["add", "foo", "--ignore-scripts"], "pnpm --ignore-scripts"),
        ],
    );
}

#[test]
fn engine_remove_update_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "remove/update",
        &[
            // remove + its aliases.
            (&["remove", "foo"], "pnpm remove <pkg>"),
            (&["rm", "foo"], "pnpm rm (alias)"),
            (&["uninstall", "foo"], "npm uninstall (alias)"),
            (&["un", "foo"], "pnpm un (alias)"),
            (&["remove", "foo", "-g"], "pnpm remove -g"),
            (&["remove", "foo", "-D"], "remove from devDependencies"),
            (&["remove", "foo", "-r"], "pnpm -r remove"),
            // update + its aliases / flags.
            (&["update", "foo"], "pnpm update <pkg>"),
            (&["up", "foo"], "pnpm up (alias)"),
            (&["update"], "pnpm update (all)"),
            (&["update", "-g"], "pnpm update -g"),
            (&["update", "--latest"], "pnpm --latest"),
            (&["update", "-L"], "pnpm -L / --latest"),
            (&["update", "-i"], "pnpm -i (interactive)"),
            (&["update", "-r"], "pnpm -r update"),
        ],
    );
}

#[test]
fn engine_misc_verbs_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "dedupe/link/why/ls/audit/outdated/import",
        &[
            (&["dedupe"], "pnpm dedupe"),
            (&["dedupe", "-r"], "pnpm -r dedupe"),
            (&["link", "foo"], "pnpm link <dir>"),
            (&["unlink", "foo"], "pnpm unlink"),
            (&["why", "foo"], "pnpm why <pkg>"),
            (&["outdated"], "pnpm/npm outdated"),
            (&["outdated", "-r"], "pnpm -r outdated"),
            (&["ls"], "npm ls"),
            (&["list"], "npm list (alias)"),
            (&["ls", "-g"], "npm/pnpm ls -g"),
            (&["ls", "--depth", "0"], "npm ls --depth <n>"),
            (&["ls", "--depth=0"], "npm ls --depth=<n>"),
            (&["ls", "-r"], "pnpm -r ls"),
            (&["audit"], "npm/pnpm audit"),
            (&["audit", "--fix"], "pnpm audit --fix"),
            (&["import"], "pnpm import (lockfile migration)"),
        ],
    );
}

#[test]
fn exec_dlx_grammar_accepts_documented_forms() {
    assert_all_accepted(
        "exec/dlx",
        &[
            (&["exec", "eslint"], "pnpm exec <bin> (local)"),
            (&["dlx", "cowsay"], "pnpm dlx <pkg>"),
            (&["dlx", "-p", "cowsay", "cowsay"], "pnpm dlx -p <pkg> (fetch source)"),
            (&["dlx", "--package", "cowsay", "cowsay"], "pnpm dlx --package <pkg>"),
            (&["dlx", "-y", "cowsay"], "npx-style -y (assume yes)"),
            (&["dlx", "-q", "cowsay"], "npx-style -q (quiet)"),
        ],
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// KNOWN-STILL-BROKEN — forms an incumbent documents that nub rejects today.
// Kept (ignored) so the gap is visible, not silently missing. When the named
// follow-up lands, drop the `#[ignore]` and the row joins the passing table.
// ─────────────────────────────────────────────────────────────────────────────

/// E (fork-side, `nubjs/aube`): aube's `AddArgs` lacks `-P`/`--save-prod`,
/// `--offline`, `--prefer-offline` — all DOCUMENTED by `pnpm add --help`. The
/// fix adds the flags to aube's `AddArgs` (default-preserving) via the nub-fork
/// workflow, then this test un-ignores. The install→add routing already DROPS a
/// translated `-P`/`--save-prod` (save-to-deps is the add default), so the gap
/// is only on the bare engine `add` verb, not on `install <pkg> -P`.
#[test]
#[ignore = "E: aube AddArgs missing -P/--save-prod/--offline/--prefer-offline (fork-side, nubjs/aube)"]
fn engine_add_pnpm_only_flags_blocked_on_fork() {
    assert_all_accepted(
        "add (fork-blocked)",
        &[
            (&["add", "foo", "-P"], "pnpm add -P / --save-prod"),
            (&["add", "foo", "--save-prod"], "pnpm add --save-prod"),
            (&["add", "foo", "--offline"], "pnpm add --offline"),
            (&["add", "foo", "--prefer-offline"], "pnpm add --prefer-offline"),
        ],
    );
}

/// G (fork-side, `nubjs/aube`): aube's `OutdatedArgs` lacks `-g`/`--global`,
/// which `npm outdated -h` documents (pnpm only documents `--global-dir`, and
/// "accepts" `-g` via leniency, so this divergence is vs npm). The fix adds
/// `-g` to aube's `OutdatedArgs` via the nub-fork workflow.
#[test]
#[ignore = "G: aube OutdatedArgs missing -g/--global (fork-side, nubjs/aube)"]
fn engine_outdated_global_blocked_on_fork() {
    assert_all_accepted(
        "outdated (fork-blocked)",
        &[(&["outdated", "-g"], "npm outdated -g / --global")],
    );
}
