#!/usr/bin/env bash
# One clean 4-way warm-install table: nub vs bun vs pnpm vs npm.
#
# Warm install = warm global store + frozen lockfile, node_modules wiped between
# runs (in hyperfine --prepare, excluded from timing), no network. This is the
# case the global store exists to make cheap. Each tool gets its own copy of the
# fixture so their node_modules/lockfiles never collide; each store is pre-warmed
# once (untimed) before the timed run.
#
# Usage:
#   cargo build --release -p nub-cli
#   bash tests/bench/install/run-4way.sh
#   NUB=/path/to/nub bash tests/bench/install/run-4way.sh --fixture tanstack-start --runs 12 --warmup 3
#
# Requires: hyperfine, pnpm, bun, npm on PATH; a release nub (NUB= or target/release/nub).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
NUB="${NUB:-$REPO_ROOT/target/release/nub}"
case "$NUB" in /*) ;; *) NUB="$(cd "$(dirname "$NUB")" && pwd)/$(basename "$NUB")" ;; esac

FIXTURE="tanstack-start"
RUNS=10
WARMUP=3
while [ $# -gt 0 ]; do
  case "$1" in
    --fixture) FIXTURE="$2"; shift 2 ;;
    --runs) RUNS="$2"; shift 2 ;;
    --warmup) WARMUP="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

FIX="$REPO_ROOT/tests/bench/install/fixtures/$FIXTURE"
[ -d "$FIX" ] || { echo "no fixture at $FIX" >&2; exit 1; }
[ -x "$NUB" ] || { echo "nub not built at $NUB (run: cargo build --release -p nub-cli)" >&2; exit 1; }
command -v hyperfine >/dev/null || { echo "hyperfine not found (brew install hyperfine)" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/nub-4way-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
for t in nub bun pnpm npm; do cp -R "$FIX" "$WORK/$t"; done

echo "== warming each tool's store (untimed) =="
( cd "$WORK/nub"  && env -u CI "$NUB" install --frozen-lockfile )
( cd "$WORK/bun"  && bun install --frozen-lockfile )
( cd "$WORK/pnpm" && pnpm install --frozen-lockfile --silent )
( cd "$WORK/npm"  && npm ci )

echo "== warm install: $FIXTURE =="
hyperfine --warmup "$WARMUP" --runs "$RUNS" \
  --command-name "nub install"  --prepare "rm -rf '$WORK/nub/node_modules'"  "env -u CI '$NUB' --cwd '$WORK/nub' install --frozen-lockfile -s" \
  --command-name "bun install"  --prepare "rm -rf '$WORK/bun/node_modules'"  "bun install --frozen-lockfile --cwd '$WORK/bun'" \
  --command-name "pnpm install" --prepare "rm -rf '$WORK/pnpm/node_modules'" "pnpm install --frozen-lockfile --dir '$WORK/pnpm' --silent" \
  --command-name "npm ci"       --prepare "rm -rf '$WORK/npm/node_modules'"  "cd '$WORK/npm' && npm ci --offline --ignore-scripts"
