#!/usr/bin/env bash
# Script-runner dispatch benchmark: nub run vs pnpm run vs corepack pnpm run
# Full process wall-clock on a trivial noop script so runner overhead dominates.
#
# Usage: bash tests/bench/script-runner/run-legacy.sh [--warmup N] [--runs N]
#
# What it measures: how long it takes each runner to dispatch "node -e ''"
# — a near-instant script. The *runner* startup is the signal; the script
# itself is intentional noise floor.
#
# The nub binary is a compiled Rust binary (~50MB, immediate start).
# pnpm / corepack pnpm must cold-load a large JS bundle via Node on every
# invocation — that overhead is exactly what this benchmark surfaces.
#
# Requires: hyperfine, pnpm, corepack, target/release/nub (pre-built)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
NUB="$REPO_ROOT/target/release/nub"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"

WARMUP=5
RUNS=20
SAVE_RESULTS=0
QUIET=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --warmup)        WARMUP="$2"; shift 2 ;;
    --runs)          RUNS="$2";   shift 2 ;;
    --save)          SAVE_RESULTS=1; shift ;;
    --quiet|--clean) QUIET=1;    shift ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done
if [[ "$SAVE_RESULTS" -eq 1 ]]; then
  RESULTS_DIR="$REPO_ROOT/tests/bench/script-runner/results"
else
  RESULTS_DIR="$(mktemp -d /tmp/nub-bench-results-XXXXXX)"
fi

# ── Preflight ──────────────────────────────────────────────────────────────
if [[ ! -x "$NUB" ]]; then
  echo "ERROR: $NUB not found or not executable. Run 'cargo build --release' first." >&2
  exit 1
fi
if ! command -v hyperfine &>/dev/null; then
  echo "ERROR: hyperfine not found. Install with: brew install hyperfine" >&2
  exit 1
fi
if ! command -v pnpm &>/dev/null; then
  echo "ERROR: pnpm not found." >&2
  exit 1
fi
if ! command -v corepack &>/dev/null; then
  echo "ERROR: corepack not found." >&2
  exit 1
fi

NUB_VER="$("$NUB" --version 2>&1)" && NUB_VER="${NUB_VER%%$'\n'*}"
PNPM_VER="$(pnpm --version)"
COREPACK_VER="$(corepack --version)"
NODE_VER="$(node --version)"

# ── Fixture ────────────────────────────────────────────────────────────────
# A minimal project with a trivial noop script. pnpm-lock.yaml is generated
# once so pnpm never resolves/installs at run time.
# packageManager field pins corepack to the same pnpm version on PATH.
FIXTURE="$(mktemp -d /tmp/nub-script-runner-bench-XXXXXX)"
trap 'rm -rf "$FIXTURE"' EXIT

PNPM_VER_FIELD="$PNPM_VER"
cat > "$FIXTURE/package.json" <<EOF
{
  "name": "nub-script-runner-bench",
  "version": "1.0.0",
  "scripts": {
    "noop": "node -e \"\""
  },
  "packageManager": "pnpm@${PNPM_VER_FIELD}"
}
EOF

# Generate a minimal lockfile so pnpm doesn't attempt to install anything
pnpm install --lockfile-only --dir "$FIXTURE" --silent 2>/dev/null \
  || pnpm install --lockfile-only --dir "$FIXTURE" 2>&1 | tail -2

# Verify all three tools work before we benchmark
# Note: all three tools require cwd to be the project root for script lookup.
( cd "$FIXTURE" && "$NUB" run noop >/dev/null 2>&1 )
( cd "$FIXTURE" && pnpm run noop >/dev/null 2>&1 )
( cd "$FIXTURE" && corepack pnpm run noop >/dev/null 2>&1 )

# ── Run benchmark ──────────────────────────────────────────────────────────
if [[ "$QUIET" -eq 0 ]]; then
  echo "================================================================"
  echo "  Script-runner dispatch benchmark"
  echo "  Script: 'node -e \"\"'  (trivial noop — runner overhead dominates)"
  echo "  nub:      $NUB_VER  ($NUB)"
  echo "  pnpm:     $PNPM_VER"
  echo "  corepack: $COREPACK_VER"
  echo "  node:     $NODE_VER"
  echo "  warmup: $WARMUP  runs: $RUNS"
  echo "  date: $(date)"
  echo "================================================================"
  echo ""
  echo "  NOTE: Check 'uptime' before trusting these numbers."
  echo "  High load averages contaminate wall-clock timings."
  echo ""
fi

OUTFILE="$RESULTS_DIR/script-runner-${TIMESTAMP}.json"
mkdir -p "$RESULTS_DIR"

hyperfine \
  --warmup "$WARMUP" \
  --runs "$RUNS" \
  --export-json "$OUTFILE" \
  "cd '$FIXTURE' && $NUB run noop" \
  "cd '$FIXTURE' && pnpm run noop" \
  "cd '$FIXTURE' && corepack pnpm run noop"

if [[ "$QUIET" -eq 0 ]]; then
  echo ""
  echo "  [results saved → $OUTFILE]"
  echo ""
elif [[ "$SAVE_RESULTS" -eq 1 ]]; then
  echo "  [saved → $(basename "$OUTFILE")]"
fi
echo ""

# ── Summary ────────────────────────────────────────────────────────────────
# Parse mean/stddev from JSON using node (always available in this repo).
node -e "
const r = require('$OUTFILE').results;
const [nub, pnpm, cp] = r;
const ms = v => (v * 1000).toFixed(1);
const ratio = (a, b) => (a / b).toFixed(2);
console.log('');
console.log('  Summary (mean ± σ):');
console.log('    nub run:            ' + ms(nub.mean) + 'ms ± ' + ms(nub.stddev) + 'ms');
console.log('    pnpm run:           ' + ms(pnpm.mean) + 'ms ± ' + ms(pnpm.stddev) + 'ms');
console.log('    corepack pnpm run:  ' + ms(cp.mean) + 'ms ± ' + ms(cp.stddev) + 'ms');
console.log('');
console.log('  Ratios (full wall-clock):');
console.log('    nub vs pnpm:          ' + ratio(pnpm.mean, nub.mean) + 'x faster');
console.log('    nub vs corepack pnpm: ' + ratio(cp.mean, nub.mean) + 'x faster');
console.log('');
console.log('  Headline: nub run dispatches scripts in ~' + Math.round(nub.mean * 1000) + 'ms vs pnpm\\'s ~' + Math.round(pnpm.mean * 1000) + 'ms — ' + ratio(pnpm.mean, nub.mean) + 'x faster.');
"

echo "================================================================"
echo "  Done."
echo "================================================================"
