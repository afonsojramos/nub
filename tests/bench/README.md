# Install benchmark harness

Apples-to-apples wall-clock comparison of `nub install` vs `pnpm install`, `bun install`, and `npm ci` on frozen lockfiles. The primary measurement is the **warm** install: warm CAS store + lockfile present, `node_modules` wiped, then a full offline reinstall — the repeated-checkout / CI-restore scenario.

## The benchmarks (read this first)

Three benchmarks make up the suite. Run each on a QUIET machine (see the load warning below); the latest measured values live in the gitignored `tests/bench/RESULTS.md`.

| Benchmark | Script | What it measures |
|-----------|--------|----------------|
| Warm install — GVS eligible | `run-warm-gvs.sh --fixture gvs-eligible` | `nub install` warm-install time vs pnpm, on a project where nub's global virtual store stays ON. |
| Warm install — GVS ineligible | `run-warm-gvs.sh --fixture gvs-ineligible` | With `next` present, GVS auto-disables and nub ≈ pnpm. |
| Script-runner dispatch | `run-script-runner-vs-node.sh` | `nub run` dispatch overhead vs `node --run`, npm, and pnpm on pure-shell and empty-Node script bodies. |

### Warm install — the GVS-eligibility split

`nub install`'s warm-install speed comes from its **global virtual store (GVS)**: `node_modules` is a per-project isolated layout (`node_modules/<pkg> → .nub/<pkg>/node_modules/<pkg>`), but with GVS ON the inner package under `.nub/` is **hardlinked from one shared store** (`~/.cache/aube/virtual-store`) rather than materialized per-project. A warm re-install becomes a relink against an already-materialized store. GVS defaults ON outside CI.

**The GVS-eligibility caveat.** nub force-disables GVS when any of a fixed set of frameworks is a dependency *anywhere* in the project, because they break under the shared store. **nub's trigger list is `next, nuxt, parcel`** (wired in `crates/nub-cli/src/pm_engine/mod.rs` via the aube embedder-defaults seam). When one is present, nub prints a one-line `WARN_NUB_GVS_INCOMPATIBLE` warning and falls back to per-project materialize — roughly pnpm parity.

> **`vite`, `vitepress`, and `@sveltejs/kit` are NOT triggers in nub.** They are in *aube's* standalone default list, but nub overrides that list, so those apps keep GVS ON. The auto-disable path is therefore exercised with **`next`**, not vite. GVS stays ON for: backends, libraries, plain-Node projects, Vite/VitePress apps, SvelteKit apps, Webpack apps — everything except Next/Nuxt/Parcel projects.

The two fixtures encode exactly this split:

- `fixtures/gvs-eligible/` — ~571 resolved packages (express, koa, fastify, lodash, typescript, eslint, prettier, vitest, drizzle-orm, …), **no** trigger framework. GVS stays ON.
- `fixtures/gvs-ineligible/` — the same set **plus `next`** (+react/react-dom). GVS auto-disables; the GVS-on speedup does not apply to Next-class apps.

Verify the linking path nub actually took (the harness prints this): the real GVS signal is **not** the top-level symlink (identical in both modes) but the inner package's hardlink count — `stat -f '%l' node_modules/.nub/lodash@*/node_modules/lodash/package.json`. nlink ≥ 2 → hardlinked from the shared store → GVS ON; nlink == 1 → materialized per-project → GVS OFF.

```bash
NUB=/path/to/target/release/nub bash tests/bench/run-warm-gvs.sh
# or one fixture:
NUB=… bash tests/bench/run-warm-gvs.sh --fixture gvs-eligible --runs 12 --warmup 3
```

### Script-runner dispatch — `nub run` vs `node --run`

The canonical script-runner benchmark measures how fast each tool looks up a `package.json` script and dispatches it. It runs two no-dependency fixtures: a pure-shell script (`"noop": "true"`) that isolates runner dispatch, and an empty Node body (`"noop": "node -e \"\""`) that shows how much the dispatch delta is diluted once the script itself also boots Node. The checked-in script is a thin `hyperfine` wrapper: it creates the fixtures, verifies every command exits 0 before timing, then runs `hyperfine` and writes JSON to `tests/bench/results/`.

```bash
git submodule update --init --depth 1 vendor/aube
cargo build --release -p nub-cli
NUB="$PWD/target/release/nub" bash tests/bench/run-script-runner-vs-node.sh --runs 100 --warmup 10
```

For a quick smoke test, lower the sample count and bypass the quiet-machine gate:

```bash
NUB=target/release/nub bash tests/bench/run-script-runner-vs-node.sh --runs 1 --warmup 0 --max-load 999
```

The wrapper runs this `hyperfine` shape for each fixture:

```bash
hyperfine --warmup "$WARMUP" --runs "$RUNS" --export-json "$OUT" \
  --command-name "nub run"    "cd '$FIXTURE' && '$NUB' run noop" \
  --command-name "node --run" "cd '$FIXTURE' && node --run noop" \
  --command-name "npm run"    "cd '$FIXTURE' && npm run noop" \
  --command-name "pnpm run"   "cd '$FIXTURE' && pnpm run noop"
```

The older `run-script-runner-pure.sh` and `run-script-runner.sh` harnesses are kept for historical comparisons against npm/pnpm-only runs; use `run-script-runner-vs-node.sh` for the published `node --run` comparison.

### Correctness gotcha: the timed command must start with NO node_modules (the bun no-op)

A frozen/offline install that finds `node_modules` already present can **short-circuit and no-op** — bun does exactly this, returning in ~0–40 ms without rebuilding anything, which produces a bogus "bun is faster" reading. The `--prepare` reset must therefore guarantee `node_modules` is *gone* before each timed run. `run-warm-gvs.sh` renames `node_modules` to a unique per-iteration trash slot, reaps that slot in the background (off the timed path), and then **busy-waits until `node_modules` no longer exists** before `--prepare` returns. The earlier shared-glob `rm -rf $TRASH/r-* &` raced the next iteration's rename and left `node_modules` half-present, which let bun no-op — fixed. nub and pnpm do *not* no-op (their install state lives inside `node_modules/.nub` / `node_modules/.pnpm`, so wiping `node_modules` forces a genuine relink), but the reset is made correct for all tools regardless.

## Quick run

```bash
cd /path/to/dun
cargo build --release -p nub-cli   # rebuild if source changed
bash tests/bench/run.sh
```

Results print to stdout and save to `tests/bench/results/`.

```bash
bash tests/bench/run.sh --fixture t3 --warm-only   # one fixture, warm only
bash tests/bench/run.sh --materialized             # warm leg with GVS off (see below)
```

**Run it on a QUIET machine.** Install wall-clock is dominated by filesystem syscalls; a loaded box (parallel builds, Spotlight indexing, other agents) inflates every number and widens σ. Check `uptime` first — load average should be near the idle baseline before you trust a headline number.

## What it measures

**Tools:** `target/release/nub` (pre-built) vs system `pnpm`, `bun` (where a `bun.lock` exists), and `npm` (where a `package-lock.json` exists). All run frozen / offline.

**Fixtures:**

| Fixture  | Packages | Description |
|----------|----------|-------------|
| simple   | ~342     | Single-package project: express, react, typescript, vite, lodash, axios, zod, … |
| monorepo | ~407     | 4-workspace monorepo (api, web, shared, utils) using `workspace:*` — tests the per-workspace linking path. **npm is skipped** (npm does not speak the `workspace:*` protocol). |
| t3       | ~222     | Bun's create-t3-app benchmark — Next 16, tRPC 11, Drizzle, next-auth 5, Tailwind 4. Source: `.repos/bun/bench/install/`. |
| large    | ~1168    | react + MUI + webpack + babel + ts + eslint — the file-count-heavy case. |

**Conditions:**

- **Warm** (primary): every tool's global store pre-populated; only `node_modules` is wiped between runs, then a full offline reinstall is timed. 3 warmup + 12 timed iterations via `hyperfine`.
- **Cold**: empty per-run stores; first-install from registry. 5 manual iterations (network I/O dominates). Store isolation: pnpm `--store-dir`; nub `XDG_DATA_HOME` + `XDG_CACHE_HOME`; bun `BUN_INSTALL_CACHE_DIR`.

## Why the warm number is sound — teardown is NOT timed

The first concern with any warm-install benchmark is that wiping `node_modules` between runs is itself slow and could dilute the timed install. **It does not, by construction:**

`hyperfine`'s `--prepare` command runs *before each timed run* and is **excluded** from the measurement — only the benchmarked command's own wall-clock is recorded (confirmed against `hyperfine --help`, 1.15.0: *"--prepare … Execute CMD before each timing run."*). The harness gives **each tool its own `--prepare`**, so each tool's teardown is isolated to that tool and never counted in any timing — not its own, not another tool's. The stopwatch covers the install and nothing else.

### Teardown cost is wildly asymmetric — so we rename-aside

Even though teardown is untimed, it still costs *wall-clock*, and the cost is not uniform:

| Tool | warm `node_modules` shape | size (simple) | `rm -rf` time |
|------|---------------------------|---------------|---------------|
| nub (GVS on) | symlink farm into `~/.cache/aube/virtual-store/` | ~260K | ~100 ms |
| nub (materialized) / pnpm / npm | real files / hardlinks under `node_modules/.pnpm/` etc. | 140–540 MB | **1–12 s** |

pnpm's `rm -rf node_modules` on the `large` fixture is **12.3 seconds**. At 12 timed runs × multiple fixtures, paying that in pure (untimed) deletion makes the suite impractical — repeated install/delete cycles would let deletion cost dominate.

The fix is **rename-aside + background reap**: the `--prepare` step `mv`s `node_modules` into a trash dir (an atomic rename, ~50 ms even for the 540 MB materialized case) and fires a detached `rm -rf` that runs off the critical path. The install never sees stale state; wall-clock stays bounded regardless of how large a tool's `node_modules` is.

The harness prints each tool's rename-aside teardown cost once per fixture, under `[teardown cost — untimed, reported for transparency]`, so the saving is visible and the separation is auditable. These lines are **not** part of any timed number.

## GVS state is controlled explicitly, not inherited

nub's **global virtual store** (GVS) — the shared virtual-store cache that lets nub's warm install be a symlink farm — defaults to **on outside CI** and **off inside CI**. That default is intentionally not a user-facing pinning surface: use the existing install config setting instead.

For a project-level pin, write the setting directly:

```ini
enableGlobalVirtualStore=false
```

For a one-command benchmark override, use the npm config env alias:

```bash
npm_config_enable_global_virtual_store=false nub install
```

The benchmark harness scrubs ambient `CI` for default warm runs so a developer's shell or CI runner cannot silently flip the result. The materialized leg should be understood as **GVS off via `enableGlobalVirtualStore=false`**, not as a new nub CLI flag or a recommendation to use `CI` as a user knob.

The CAS store and the GVS itself are **never cleared** between runs — clearing them would make the run *cold*, not warm. Warm means the store is populated; only `node_modules` is reset.

## Honest reporting

σ-overlap between nub and another tool is a TIE, not a win. Report the median and σ, run on a quiet machine, and don't quote a single best run as the headline.

## Re-running after a perf change

1. `cargo build --release -p nub-cli` to rebuild the binary.
2. `bash tests/bench/run.sh` (on a quiet machine).

Compare against the committed `results/` snapshot.

## Fixtures

Pre-generated under `fixtures/`. Lockfiles are committed so all tools resolve identically:

- `pnpm-lock.yaml` (nub + pnpm), `bun.lock` (bun), `package-lock.json` (npm).

Each tool's workdir is set up with only its own lockfile family; foreign lockfiles are stripped so no tool refuses to infer a PM or reads a stale lock.

To regenerate the pnpm/bun lockfiles:

```bash
bash tests/bench/gen-fixtures.sh
```

To regenerate npm lockfiles (npm has no `--frozen` lockfile-only mode that round-trips the others' resolutions, so generate per fixture):

```bash
for f in simple t3 large; do ( cd tests/bench/fixtures/$f && npm install --package-lock-only --ignore-scripts ); done
# monorepo uses workspace:* — npm cannot install it, so it has no package-lock.json.
```
