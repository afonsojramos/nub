#!/usr/bin/env bash
# Script-runner DISPATCH benchmark — PURE-SHELL script (no node invocation).
#
# What it measures: how long each runner takes to look up a script in
# package.json and dispatch it. To isolate the RUNNER's overhead, the script
# itself MUST be pure-shell (`true` / `echo hi`) — NEVER `node -e ...`.
# Node's ~40ms cold startup would swamp the few-ms runner overhead and dilute
# the very thing being measured.
#
# Tools: nub run / pnpm run / npm run; bun run optional (see README).
#
# Requires: hyperfine, pnpm, npm; bun optional; NUB env var or target/release/nub.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
NUB="${NUB:-$REPO_ROOT/target/release/nub}"
# Resolve NUB to an absolute path: every install/run happens in a subshell that
# cd's into a fixture dir, so a relative NUB= (e.g. target/release/nub) would
# fail to resolve there. Tolerate either a relative or absolute override.
case "$NUB" in /*) ;; *) NUB="$(cd "$(dirname "$NUB")" 2>/dev/null && pwd)/$(basename "$NUB")" ;; esac
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"

WARMUP=5
RUNS=30
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

[[ -x "$NUB" ]] || { echo "ERROR: nub not found at $NUB" >&2; exit 1; }
command -v hyperfine &>/dev/null || { echo "ERROR: hyperfine not found." >&2; exit 1; }
command -v pnpm &>/dev/null || { echo "ERROR: pnpm not found." >&2; exit 1; }
command -v npm &>/dev/null || { echo "ERROR: npm not found." >&2; exit 1; }
HAS_BUN=0; command -v bun &>/dev/null && HAS_BUN=1

# Fixture: a project whose only script is PURE SHELL (`true`). No deps, no
# lockfile install needed — the script never touches node.
FIXTURE="$(mktemp -d /tmp/nub-script-runner-pure-XXXXXX)"
trap 'rm -rf "$FIXTURE"' EXIT
cat > "$FIXTURE/package.json" <<'EOF'
{
  "name": "script-runner-pure-bench",
  "version": "1.0.0",
  "scripts": {
    "noop": "true"
  }
}
EOF

# Verify each runner can dispatch the script (cwd must be project root).
( cd "$FIXTURE" && "$NUB" run noop >/dev/null 2>&1 ) || { echo "nub run failed" >&2; exit 1; }
( cd "$FIXTURE" && pnpm run noop >/dev/null 2>&1 ) || { echo "pnpm run failed" >&2; exit 1; }
( cd "$FIXTURE" && npm run noop >/dev/null 2>&1 )  || { echo "npm run failed" >&2; exit 1; }

if [[ "$QUIET" -eq 0 ]]; then
  echo "================================================================"
  echo "  Script-runner DISPATCH benchmark — PURE-SHELL script ('true')"
  echo "  (no node invocation — isolates runner overhead)"
  echo "  nub:  $("$NUB" --version 2>&1 | head -1)"
  echo "  pnpm: $(pnpm --version)   npm: $(npm --version)"
  echo "  bun:  $([[ $HAS_BUN -eq 1 ]] && bun --version || echo '(absent)')"
  echo "  warmup: $WARMUP  runs: $RUNS"
  echo "  date: $(date)   load: $(uptime | sed 's/.*load averages*: //')"
  echo "================================================================"
fi

OUTFILE="$RESULTS_DIR/script-runner-pure-${TIMESTAMP}.json"
mkdir -p "$RESULTS_DIR"

HF_ARGS=(
  --warmup "$WARMUP" --runs "$RUNS" --export-json "$OUTFILE"
  --command-name "nub run"  "cd '$FIXTURE' && '$NUB' run noop"
  --command-name "pnpm run" "cd '$FIXTURE' && pnpm run noop"
  --command-name "npm run"  "cd '$FIXTURE' && npm run noop"
)
if [[ $HAS_BUN -eq 1 ]]; then
  ( cd "$FIXTURE" && bun run noop >/dev/null 2>&1 ) \
    && HF_ARGS+=( --command-name "bun run" "cd '$FIXTURE' && bun run noop" )
fi

hyperfine "${HF_ARGS[@]}"
if [[ "$QUIET" -eq 0 ]]; then
  echo ""
  echo "  [results saved → $OUTFILE]"
  echo "================================================================"
elif [[ "$SAVE_RESULTS" -eq 1 ]]; then
  echo "  [saved → $(basename "$OUTFILE")]"
fi
