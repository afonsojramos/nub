# Package install benchmarks

Wall-clock comparison of `nub install` vs `pnpm install`, `bun install`, and `npm ci` on frozen lockfiles. The primary measurement is the warm install: warm CAS store + lockfile present, `node_modules` wiped, then a full offline reinstall.

## Quick run

```bash
cd /path/to/dun
cargo build --release -p nub-cli
bash tests/bench/install/run-warm-gvs.sh
```

For the older fixture matrix:

```bash
bash tests/bench/install/run.sh --fixture t3 --warm-only
bash tests/bench/install/run.sh --materialized
```

## Warm install — GVS eligibility

`nub install`'s warm-install speed comes from its global virtual store. `node_modules` stays project-local, but with GVS on the inner package under `.nub/` is hardlinked from a shared store instead of materialized per project. A warm reinstall becomes a relink against an already-materialized store.

The main harness exercises both sides of the compatibility split:

| Fixture | Script | What it measures |
|---------|--------|------------------|
| GVS eligible | `run-warm-gvs.sh --fixture gvs-eligible` | `nub install` warm-install time vs pnpm where GVS stays on. |
| GVS ineligible | `run-warm-gvs.sh --fixture gvs-ineligible` | A `next` project where GVS auto-disables and nub is roughly pnpm parity. |

Nub's trigger list is `next`, `nuxt`, and `parcel`. `vite`, `vitepress`, and `@sveltejs/kit` are not triggers in Nub.

```bash
NUB=/path/to/target/release/nub bash tests/bench/install/run-warm-gvs.sh
NUB=/path/to/target/release/nub bash tests/bench/install/run-warm-gvs.sh --fixture gvs-eligible --runs 12 --warmup 3
```

## Older install matrix

The older `run.sh` matrix covers frozen/offline warm and cold installs across these fixtures.

| Fixture | Packages | Description |
|---------|----------|-------------|
| `simple` | ~342 | Single-package project: express, react, typescript, vite, lodash, axios, zod, … |
| `monorepo` | ~407 | Four-workspace monorepo using `workspace:*`; npm is skipped. |
| `t3` | ~222 | Bun's create-t3-app benchmark fixture (Next — GVS auto-disables). |
| `tanstack-start` | ~313 | Real TanStack Start (Vite + React) starter; GVS stays on. See `fixtures/tanstack-start/README.md`. |
| `large` | ~1168 | React + MUI + webpack + Babel + TypeScript + ESLint. |

```bash
bash tests/bench/install/run.sh
bash tests/bench/install/run.sh --fixture t3 --warm-only
bash tests/bench/install/run.sh --cold-only
```

## Results

By default, scripts write JSON to a temp directory. Pass `--save` to update checked-in JSON under `tests/bench/install/results/`.

## Methodology notes

The timed warm command starts with no `node_modules`. The harness uses hyperfine `--prepare` to rename `node_modules` aside before each timed run, then reaps it in the background. Teardown is not timed.

The CAS store and GVS are never cleared between warm runs. Clearing them would make the run cold, not warm.

Report median and σ. σ-overlap between tools is a tie, not a win.

## Fixtures

Fixtures live under `tests/bench/install/fixtures/`. Lockfiles are committed so each tool resolves consistently:

- `pnpm-lock.yaml` for Nub and pnpm
- `bun.lock` for Bun
- `package-lock.json` for npm where supported

Regenerate pnpm and Bun lockfiles:

```bash
bash tests/bench/install/gen-fixtures.sh
```

Regenerate npm lockfiles:

```bash
for f in simple t3 large; do ( cd tests/bench/install/fixtures/$f && npm install --package-lock-only --ignore-scripts ); done
```
