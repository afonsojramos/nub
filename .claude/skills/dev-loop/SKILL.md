---
name: dev-loop
description: >-
  Build and test nub during development. Invoke (via the Skill tool) whenever
  you need to compile the dev `nub` binary, set up a worktree for fast
  incremental iteration, run a specific test file or a single test, or get
  oriented in the codebase (the crate map). Encodes the measured fast-build
  loop: the `fast` profile + ONE shared CARGO_TARGET_DIR across all worktrees
  (`~/.cache/nub/shared-target`) — deps are reused, only the workspace crates
  recompile, and the disk cost is one target dir instead of ~30. A shared
  cross-worktree compiler-WRAPPER cache (sccache) was separately measured to
  give 0% Rust speedup and is NOT used; a shared cargo target DIR is a
  different mechanism and is the default. Covers the real incantations (`cargo
  build -p nub-cli --profile fast`, `make install-dev`, `make addon-fast`), the
  test invocations, and the exact CI cheap gates.
---

# Building & testing nub

nub is a Rust workspace — three crates (`nub-cli`, `nub-core`, `nub-native`) plus the vendored aube PM engine (`vendor/aube`, plain in-tree files since Pattern B, its own Cargo workspace, linked in-process as a library). This skill is the fast, measured way to build and test it in a worktree, plus a crate map so you know where things live.

**The one rule that makes iteration fast:** build with the `--profile fast` profile (NEVER `release`), pointing CARGO_TARGET_DIR at the ONE shared dir `~/.cache/nub/shared-target` (`new-worktree.ts` prints exactly this). Cold ≈ 3 min, every rebuild after ≈ 5s. Don't clean the shared dir between iterations — that throws away cargo's incremental cache and forces a full rebuild. The shared dir means a second worktree reuses all the crates.io dependency artifacts (the bulk of a build) and recompiles only the ~10 workspace crates. Tradeoff: cargo takes a build lock on the target dir, so a build in one worktree waits for a concurrent build in another — the deliberate cost of not carrying ~30 multi-GB private target dirs.

---

## Step 1 — Set up a worktree to iterate in

Substantive work lands via a PR from an isolated worktree. Create one with the `worktree` skill (`nub scripts/new-worktree.ts <slug>` — it bakes in the proven `git worktree add … origin/main` recipe). Then, for the build context, point CARGO_TARGET_DIR at the shared dir the script prints:

```bash
cd ~/.cache/nub/worktrees/<slug>
export CARGO_TARGET_DIR=~/.cache/nub/shared-target             # ONE shared cache for all worktrees
```

The shared target dir means cargo reuses the crates.io dependency artifacts (the bulk of a build) that another worktree already compiled — a fresh worktree recompiles only the ~10 workspace crates instead of all ~400. It also replaces ~30 multi-GB private target dirs with one. Tradeoff: cargo locks the target dir during a build, so a build in one worktree waits while another worktree is building — the deliberate cost of the disk win. (If you genuinely need two builds to run at once, set a private `CARGO_TARGET_DIR` for the second — but the shared dir is the default.)

## Step 2 — Build the dev binary (the `fast` profile)

```bash
# The dev CLI binary -> target/fast/nub. This is the iteration build.
cargo build -p nub-cli --profile fast

# Full dev binary + N-API addon, symlinked on PATH as nub-dev / nubx-dev:
make install-dev        # runs addon-fast, then `cargo build --profile fast`, then symlinks target/fast/nub

# Just the native addon (oxc transpiler), fast profile:
make addon-fast         # -> runtime/addons/nub-native.node
# Release-profile addon (only when you specifically need release behavior):
make addon
```

There is **no `nub build` command** — the dev build is `cargo build -p nub-cli --profile fast` (or `make install-dev` for the full binary+addon on PATH).

**Why `fast`, never `release`, for iteration** (measured 2026-06-20, macOS arm64):

| build | wall time |
|---|---|
| `--profile fast`, cold, empty shared target dir | **~3 min** |
| `--profile fast`, fresh worktree against a WARM shared target dir | only the ~10 workspace crates recompile (deps reused) |
| `--profile fast`, rebuild after a 1-file change, same target dir | **~5s** |
| `--profile release`, cold | **~15 min** (and re-LTOs the whole binary on every change) |

The `fast` profile (defined in `Cargo.toml`) inherits `dev` (debug-assertions + overflow checks stay on), drops LTO, uses `codegen-units=256`, line-tables-only debuginfo, and `incremental=true`. It is the iteration profile; `release` is a ship profile and must not be used to iterate.

**A shared cargo target DIR (the default here) is NOT sccache — don't conflate them.** sccache (a compiler-WRAPPER cache) was measured against this workspace and gives a **0% Rust cache-hit rate** across separate target dirs (rustc embeds per-target-dir artifact paths in sccache's cache keys; `--remap-path-prefix` + `CARGO_INCREMENTAL=0` does not fix it) — so sccache is NOT used. A shared cargo *target dir* sidesteps that entirely: with one target dir there's a single artifact path, so cargo's own incremental reuses the dependency rlibs directly across worktrees. That is why the default is `~/.cache/nub/shared-target`, not a per-worktree dir. (Seeding a private worktree target dir from a warm sibling via APFS clone is still useless — cargo invalidates the cloned fingerprints and rebuilds; the shared dir avoids the copy in the first place.)

## Step 3 — Run tests

```bash
# A specific integration-test file (file stem under crates/nub-cli/tests/):
cargo test -p nub-cli --test pm_verbs
cargo test -p nub-cli --test install_engine

# A single test by name substring (across the crate):
cargo test -p nub-cli <substring>
# Pin exactly one test:
cargo test -p nub-cli -- --exact <full::module::path::to::test>

# A core/native crate's tests:
cargo test -p nub-core
# nub-native is its OWN workspace (excluded from the root one), so `-p nub-native`
# from the repo root fails — run it from inside the crate. (The cdylib sets
# `test = false`, so this just compiles the addon; its unit-testable logic lives
# in the napi-free nub-cache-key crate, covered by `cargo test -p nub-cache-key`.)
(cd crates/nub-native && cargo test)

# Everything (slow):
cargo test          # or `make test`
```

The `nub-cli` integration suite lives in `crates/nub-cli/tests/*.rs` — e.g. `pm_verbs`, `install_engine`, `info_engine`, `cli_grammar_parity`, `pm_identity`, `pm_two_mode`, `resolution_compat`, `node_compat`, `version_tiers`, `workspace_run`, the `pm_shim*` / `*_config` files. Use the file stem as `--test <stem>`.

## Step 4 — Before pushing: the exact CI cheap gates

Match `.github/workflows/ci.yml` exactly — a scoped `-p` without `--all-targets` misses test-code lints:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo test -p <crate>        # scoped to what you changed
```

Then run the full [pre-push local verification loop in AGENTS.md](../../../AGENTS.md) (incremental build → exact CI gates → e2e tmp-fixture run → Docker for global-cache/config behavior → promote durable checks into the suite). For the e2e probe loop specifically, use the `ad-hoc-test` skill. Get it green locally and push ONCE — fix-after-fix pushes saturate the shared CI runner pool.

---

## Crate map — where things live

**`crates/nub-cli`** — the CLI (clap dispatch + PM verb routing).
- `src/cli.rs` — the clap command grammar + dispatch (the pnpm-compatible PM surface, `run`/`watch`/`nubx`/`upgrade`/`node`, the top-level file runner).
- `src/main.rs` — entry point.
- `src/pm_engine/` — routes PM verbs into the vendored aube engine in-process. `mod.rs` (`ENGINE_VERBS`), `present.rs` (rebrands engine output: `ERR_AUBE_*`→`ERR_NUB_*`, `aube`→`nub`), `config_scope.rs` (mirror-active-PM / brand-boundary config policy), `identity.rs` (PM-identity inference), `install_family.rs`, `info_family.rs`, `publish_family.rs`, `store_config_family.rs`, `use_*.rs`, and `bun_config.rs` / `yarn_*` / `unsupported_config.rs` for incumbent-PM compat.
- `src/agent/` — agent surface.
- `tests/*.rs` — integration tests.

**`crates/nub-core`** — runtime/orchestration.
- `src/node/` — Node integration: `discovery.rs` (find the user's Node on PATH), `version.rs` (version management), `flags.rs` (V8 / Node flag injection), `feature_matrix.rs` (tier + Node-version gating — the source of truth for version-gated feature claims), `spawn.rs` (process spawn), `mod.rs`.
- `src/pm/`, `src/workspace/`, `src/version_management/`.
- `src/pnp.rs` — Yarn PnP support.

**`crates/nub-native`** — the N-API addon (a cdylib loaded into the user's Node process). The oxc-based transpiler + resolver: `transform.rs` (TS/JSX transform), `resolve.rs` (module resolution), `tsconfig.rs`, `cache.rs` (transpile cache), `detect.rs`.

**`vendor/aube`** — the vendored aube package-manager engine (plain in-tree files since Pattern B, vendored from `nubjs/aube`). Its own Cargo workspace; nub takes path deps into `vendor/aube/crates/*` and calls `aube::commands::<verb>::run(...)` in-process. NEVER a subprocess. From a build standpoint it's just part of the workspace — `cargo build` compiles it as a dependency. Changes to it are normal nub edits/PRs touching `vendor/aube/*` (no pin, no submodule). For pulling FROM / pushing TO upstream `jdx/aube`, see the `aube-sync` skill.

---

## Quick reference

```bash
# fresh worktree (see the `worktree` skill: nub scripts/new-worktree.ts <slug>)
cd ~/.cache/nub/worktrees/<slug> && export CARGO_TARGET_DIR=~/.cache/nub/shared-target  # ONE shared cache

# build (fast profile)
cargo build -p nub-cli --profile fast          # -> target/fast/nub  (~3 min cold, ~5s incremental)
make install-dev                                # full binary + addon on PATH as nub-dev/nubx-dev
make addon-fast                                 # native addon only

# test
cargo test -p nub-cli --test <file_stem>        # one file
cargo test -p nub-cli <substring>               # one test by name

# CI cheap gates
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```
