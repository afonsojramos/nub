---
name: benchmarking
description: Comparative install-benchmarking methodology for nub vs npm/pnpm/bun — cold/warm protocol, genuine-cold cache isolation, load-robust measurement, and the anti-juicing honesty bar. Invoke (via the Skill tool) whenever you need to benchmark `nub install` against another package manager, produce or update the homepage/blog install numbers, or verify a perf claim before it ships. Encodes the hard-won gotchas: time setup OUTSIDE the measurement (hyperfine `--prepare`), the cache lives on DISK so env-var isolation is NOT trustworthy (bun ignores `BUN_INSTALL_CACHE_DIR`/`$HOME` — wipe the real path), VERIFY every cold is genuine via an offline-fails check, and lead with load-independent signals (file counts, store-entry counts) on this permanently-contended host. Pairs with `pm-perf-tracing` for the internal Rust phase decomposition.
---

# benchmarking

How to run an honest, reproducible **comparative install benchmark** of `nub install` against npm / pnpm / bun. This is the EXTERNAL, wall-clock-and-file-count method (one tool vs another on an identical fixture). For decomposing where the time goes INSIDE a single nub install — `phase:resolve/fetch/link`, the per-file linker strategy tally — use the complementary [`pm-perf-tracing`](../pm-perf-tracing/SKILL.md) skill instead.

## When this applies

Benchmarking `nub install` against another PM; producing or refreshing the homepage/blog install numbers; verifying a perf claim before it ships. A single non-genuine cell discredits the whole table — so every number below is gated by the verification step.

## The tool: hyperfine

`/opt/homebrew/bin/hyperfine` is the canonical timer. The load-bearing flag is `--prepare`, which runs setup BEFORE each timed run, UNTIMED — this is how cache-clear and `node_modules`-wipe stay OUT of the measurement. The #1 hygiene rule: NEVER time setup.

```sh
# COLD: empty the tool's REAL cache + wipe node_modules before each run (untimed), then time the install.
hyperfine --warmup 0 --runs 5 \
  --prepare 'rm -rf node_modules && rm -rf "$(bun pm cache)"' \
  'bun install --ignore-scripts'

# WARM-RELINK: cache populated, wipe ONLY node_modules before each run.
hyperfine --warmup 1 --runs 5 \
  --prepare 'rm -rf node_modules' \
  'nub install --ignore-scripts'

# WARM-SAT: node_modules already present (idempotency path) — no prepare wipe.
hyperfine --warmup 1 --runs 5 'nub install --ignore-scripts'
```

hyperfine reports mean ± σ, median, and min/max. Prefer median + spread (min–max) here — contention skews the mean.

## The cold / warm protocol

`node_modules` is deleted before EVERY timed run — cold AND warm — always in `--prepare`, never timed. The cold/warm axis differs ONLY in global-cache state:

- **cold** — the tool's real cache is EMPTY (genuine download).
- **warm-relink** — cache populated, `node_modules` wiped (the link-from-store path; the homepage number).
- **warm-sat** — `node_modules` already present (re-install idempotency); a separate, clearly-labeled scenario, not the headline.

## GENUINE-COLD per tool — the cache is on DISK; env-var isolation is NOT trustworthy

A "cold" run is only cold if the tool's REAL on-disk cache is gone. Setting a cache-dir env var does not guarantee that — the biggest gotcha is **bun ignores `BUN_INSTALL_CACHE_DIR` and `$HOME`**, resolving its cache via the OS passwd home, so it cannot be cache-isolated by env. You MUST wipe the disk path.

| tool | real cache path | clear command for cold |
|---|---|---|
| nub | its store (`NUB_CACHE_DIR` + `XDG_DATA_HOME`/`XDG_CACHE_HOME`) | `rm -rf "$NUB_CACHE_DIR" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"` |
| npm | `~/.npm/_cacache` (or `--cache <dir>`) | `rm -rf <cache>` (the `--cache` dir you pass) |
| pnpm | `pnpm store path` (or `--store-dir <dir>`) | `rm -rf <store>` (or `pnpm store prune`) |
| **bun** | **`bun pm cache`** = the real `~/.bun/install/cache` | **`rm -rf "$(bun pm cache)"`** — env vars do NOT relocate it |

bun's own repo benchmarks wipe the disk path the same way — there is no env-only shortcut.

## VERIFY each cold is genuine — the offline-fails check

After wiping a tool's cache, an `--offline` install MUST FAIL. A pass means the cache wasn't actually cleared (wrong disk path), and any "cold" number from it is a warm-link artifact. Run this for every tool before trusting a cold number:

| tool | expected after cache wipe | genuine? |
|---|---|---|
| nub | `rc≠0`, "not available in the local cache" | FAIL → genuine |
| pnpm | `rc=1`, `ERR_PNPM_NO_OFFLINE_TARBALL` | FAIL → genuine |
| npm | `rc≠0`, cache-miss error | FAIL → genuine |
| bun | if `bun install --offline` SUCCEEDS → cache NOT cleared → bun cold is NOT genuine |

This is exactly what caught the bun-cache bug: with empty `BUN_INSTALL_CACHE_DIR` and a fake empty `$HOME`, `bun install --offline` still installed every package from the real 1.7 GB global cache. A true bun cold needs a clean container or wiping the user's real cache (destructive — don't, unless in Docker).

## Apples-to-apples isolation

- Each tool gets its OWN cache dir.
- `--ignore-scripts` for ALL tools (or `--allow-scripts` for all — same on both sides).
- Identical fixture, identical lockfile present throughout (use a `--frozen-lockfile`/`--frozen` equivalent where the tool offers one).
- **Interleave tool order round-robin** (nub → npm → bun → pnpm, repeat) — never all-of-one-then-the-other — so drift in host load hits every tool equally.

## Load-robustness — this host NEVER idles

The dev box load floor is ~20 and routinely 30–270 (see the machine-load memory). Absolute wall-clock σ is inflated by contention, so:

- **LEAD with load-independent signals** — exact and reproducible regardless of load: `find node_modules -type f -o -type l | wc -l` (materialized file count), store-entry counts, physical-copy counts per package (the dedup story). These often tell the story better than seconds.
- **Use back-to-back same-box RATIOS** for relative claims (nub/pnpm warm) — robust to a shared load level even when both absolutes are inflated.
- **For a PUBLISHABLE absolute number, run on a genuinely quiet box or a CI runner — NOT this host.** Never gate a measurement on the host going quiet; it won't.

## File-count forensics (the load-independent crux)

```sh
find node_modules -type f -o -type l | wc -l          # total materialized entries
ls -d node_modules/**/core-js node_modules/core-js    # physical copies of a duplicated dep (dedup story)
```

The campaign headline was a file-count delta (76,037 → 12,386, −83.7%), not a wall-clock number — exact, reproducible, and immune to load. Reach for this first.

## The honesty bar (the anti-juicing rule)

Ties directly to the repo's "never juice a benchmark" rule. Non-negotiable:

- VERIFY every cold is genuine (offline-fails check) BEFORE citing it.
- NEVER compare one tool's genuine-cold to another's warm-link — that is the exact misleading comparison the offline check exists to prevent.
- Report what was ACTUALLY measured, caveats included (which colds are genuine, host load during the run, sample size).
- The homepage cites the WARM number because it is the honest, reproducible one (genuine cold needs a clean box).
- A single non-genuine cell discredits the whole table. When in doubt, exclude the cell and say why.

## Reference template

`/tmp/cs-bench-final.sh` is a working 4-tool harness (nub / npm / bun / pnpm) — per-tool isolated cache dirs, `--ignore-scripts` for all, interleaved round-robin order, `node_modules` wiped before every run, median/spread helpers. Read its structure before hand-rolling a new one; adapt the cache paths and fixture, keep the protocol.

## Internal decomposition

When the comparative number raises "WHY is nub's phase X slow?", switch to [`pm-perf-tracing`](../pm-perf-tracing/SKILL.md): `RUST_LOG=debug nub install` for the `phase:resolve/fetch/link` split, and the gated `AUBE_DIAG_FILE` per-file linker strategy tally for link-phase questions. That skill decomposes one nub install; this one compares nub against other tools.
