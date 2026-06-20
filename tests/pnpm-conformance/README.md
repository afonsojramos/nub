# pnpm conformance harness — run pnpm's OWN test suite against nub

This harness runs pnpm's own black-box CLI test suite against the nub binary. It is the widest-net way to verify nub's pnpm-compatibility claim: instead of writing nub-authored parity tests, it points the *incumbent's* suite at nub and treats every divergence as a candidate finding. It is the PM-CLI analog of the Node-test-suite leverage harness (`tests/node-suite/`).

## The seam

pnpm's front-door package — the package literally named `pnpm` inside the [pnpm/pnpm](https://github.com/pnpm/pnpm) monorepo — ships ~64 test files in `pnpm/test/*.ts`. 63 of them spawn the real binary through ONE seam in `pnpm/test/utils/execPnpm.ts`:

```ts
export const binDir = path.join(__dirname, '../..', isWindows() ? 'dist' : 'bin')
export const pnpmBinLocation = path.join(binDir, 'pnpm.cjs')   // .mjs on newer pnpm
crossSpawn.spawn(process.execPath, [pnpmBinLocation, ...args], { env, stdio })
```

Every assertion is on stdout / stderr / exit-code / lockfile / node_modules state — exactly nub's drop-in parity surface. The harness replaces `bin/pnpm.cjs` with a shim (`nub-pnpm-shim.cjs`) that re-execs the nub binary with `argv0: 'pnpm'`, so nub adopts the pnpm identity (nub picks its package-manager role from argv[0]'s basename — `Argv0::detect` in `crates/nub-cli/src/cli.rs`). The whole suite then exercises nub.

npm's own suite is NOT usable this way: it constructs npm's internal JS `Npm` class in-process (tmock + nock), never spawning a binary (~3% black-box). The harness is pnpm-only by design.

## Why a pinned clone (not a vendored subset)

The reference checkout `.repos/pnpm` tracks pnpm's `main` (currently 11.x), which drifts from the version nub spoofs. The harness clones pnpm at a tag matching nub's pinned pnpm major (`v10.15.1`, aligned with `PNPM_PIN` in `lockfile-roundtrip.yml`) so the suite's assertions match the behavior nub targets. A vendored subset was the documented fallback if bootstrap proved too flaky; the pinned-clone bootstrap proved tractable (one `pnpm install` + a lean compile), so we use it — it never rots against the version under test.

## Files

| file | role |
| --- | --- |
| `run.sh` | the harness: clone → bootstrap → seam-swap → jest → classify |
| `nub-pnpm-shim.cjs` | the seam replacement (re-execs nub as pnpm; `__NUB_BIN__` is baked at swap time) |
| `classify.mjs` | parses jest `--json` output; classifies each failure against the allowlist |
| `allowlist.txt` | known-OK failures: intended divergences + tracked bugs |

## Running it locally

```bash
# Build nub first.
cargo build -p nub-cli

# A real pnpm must be on PATH — NOT for the commands under test (those go to nub
# via the seam), but for the suite's registry mock, which launches verdaccio via
# `pnpm --use-node-version=20.x`. Install one separate from any nub shim:
npm install -g pnpm@10.15.1

# Full suite (clones to a temp dir, pins v10.15.1):
tests/pnpm-conformance/run.sh target/debug/nub

# A single test file (fast iteration; stale-allowlist check is skipped on subsets):
tests/pnpm-conformance/run.sh target/debug/nub v10.15.1 test/root.ts

# Reuse a clone across runs (skips the slow bootstrap):
PNPM_CLONE_DIR=/tmp/pnpm-conf KEEP_CLONE=1 tests/pnpm-conformance/run.sh target/debug/nub
```

Exit 0 iff every failing test is allowlisted AND no allowlist entry is stale.

## Bootstrap, step by step

1. **Clone** pnpm/pnpm at the pinned tag (`git clone --depth 1 --branch v10.15.1`).
2. **Install** the monorepo deps (`corepack pnpm install --frozen-lockfile`, ~30 s). Corepack runs the repo's own pinned pnpm.
3. **Compile** only the `pnpm` front-door package — `tsc --build` then `bundle` (produces `pnpm/dist/pnpm.cjs`) plus the runtime-asset copies. We deliberately SKIP the full `compile-only` script: it also typechecks + lints the entire monorepo (many minutes, irrelevant to running the suite).
4. **Swap the seam**: detect whether the suite spawns `bin/pnpm.cjs` or `bin/pnpm.mjs` (version-dependent), back it up, and write the shim with the absolute nub path baked in. (Baked, not env-passed: the suite's `createEnv()` rebuilds a clean env keeping only `PATH`/`COLORTERM`/`APPDATA`, so an exported `NUB_BIN` would be stripped before the shim runs.)
5. **Run jest** scoped to `pnpm/test/` from inside the `pnpm/` package (so its `@pnpm/jest-config/with-registry` preset — which boots the registry mock — is active).
6. **Classify** the jest `--json` output against the allowlist.

## The allowlist

`allowlist.txt` lists KNOWN-OK failures as substrings matched against each failing test's full name. Two kinds of entry:

- **Intended divergences** — nub is deliberately not pnpm: `nub upgrade` is self-update (not pnpm's `update` alias); no implicit script shortcuts (nub requires the explicit `run` verb); the top-level file-run surface; the global-install root layout (`<pnpm-home>/global-nub` vs pnpm's `global/<LAYOUT_VERSION>/node_modules`).
- **Tracked bugs** — real nub bugs, allowlisted as known-failing until fixed (B1/B2/B3 in the `pnpm-compat-harness-bugs` thread). Remove a cluster the moment its fix lands.

The classifier flags two failure modes that fail the run:

- **SURPRISE** — a failing test matching NO allowlist entry: an unexpected divergence (a candidate bug).
- **STALE-ALLOW** — an allowlist entry that matched no failure (only checked on a full run): the bug may be fixed; prune the entry. This keeps the allowlist honest and shrinking.

## Flake sources (and mitigations)

- **Registry mock (verdaccio).** The jest preset boots verdaccio under Node 20 via a real pnpm. Needs a real pnpm on PATH and (first run) a Node-20 download. This is the main flake/cost source; the CI job allows 90 minutes and uploads the results JSON as an artifact.
- **Self-update banner (B3).** nub's/aube's self-update check can print an "Update available" box to stdout. The harness exports `NUB_NO_UPDATE=1` and `AUBE_NO_UPDATE_CHECK=1` to suppress it; an `Update available` entry stays in the allowlist defensively.
- **Network.** The pinned clone and the monorepo install need network; this is not an offline harness.

## CI

`.github/workflows/pnpm-conformance.yml` runs this NIGHTLY (08:00 UTC) and on-demand (`workflow_dispatch`, with optional `pnpm_tag` / `jest_args` inputs). It is intentionally NOT a per-PR gate: it is a new external-suite surface with real flake sources, so it must not block the trunk. Promote it to a per-PR gate only after it proves stable across several nightly runs.

## Proven runs (2026-06-20, local, nub v0.1.7, pnpm v10.15.1)

Full front-door suite (`pnpm/test/`, 50 suites / 337 tests, ~10 min on an M-series host):

```
PASS:          293
KNOWN-FAILING: 25   (allowlisted divergences)
SURPRISE:      0
STALE-ALLOW:   0    → exit 0 (green-or-known-failing)
```

The 25 known-failing tests cluster into three real divergence families the harness surfaced (all allowlisted pending a maintainer call — none auto-changed):

- **Global install / link / bin / `-g`** (~13 tests) — nub uses its own global layout (`<pnpm-home>/global-nub`) and does not replicate pnpm's global install/link/uninstall/`bin -g`/`root -g` surface.
- **`packageManager`-field version management** (~9 tests) — nub does not replicate pnpm's `manage-package-manager-versions` / version-switch / self-update-edits-the-field behavior. One path leaks nub-identity text where pnpm emits "This project is configured to use yarn".
- **Per-package node version, empty-dir package.json creation, dlx `.npmrc` registry** — assorted single-test divergences.

These route to the `pnpm-compat-harness-bugs` thread to decide, per family, which (if any) should converge to pnpm's behavior vs stay an intended divergence.

## Keeping the pin in sync

When nub's spoofed pnpm version changes, update in lockstep: `PNPM_TAG`/`PNPM_PIN` in the workflow, `PNPM_PIN` in `lockfile-roundtrip.yml`, and the version references in this README. A version-skew between the cloned suite and what nub targets produces false divergences.
