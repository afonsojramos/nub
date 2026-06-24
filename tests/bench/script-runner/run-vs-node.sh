#!/usr/bin/env bash
# Script-runner dispatch benchmark — nub run vs `node --run` (and npm / pnpm).
#
# This measures package.json script dispatch overhead. `node --run` starts Node
# before it can read package.json and dispatch the script. `nub run` does the
# dispatch path in Rust, then pays Node startup only if the script body invokes
# Node.
#
# Methodology: every command reads package.json, dispatches the same script, is
# warmed before measurement, and is exit-0-verified before timing.
#
# Two fixtures isolate two layers:
#   - `true`        : PURE dispatch. Both runners spawn `true` (no node body).
#                     The ONLY difference timed is whether the runner itself had
#                     to boot V8 to get there. This is the cleanest signal.
#   - `node -e ""`  : dispatch + a real (empty) node body. Both pay one node
#                     startup for the body; nub additionally avoids a SECOND
#                     node startup for the dispatch, node --run does not.
#
# This is a canonical checked-in benchmark harness. By default, result JSON is
# written to a temp directory; pass --save to update tests/bench/script-runner/results/.
#
# Requires: hyperfine, node (>=22 for `node --run`), npm, pnpm; NUB env var or
# target/release/nub.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
NUB="${NUB:-$REPO_ROOT/target/release/nub}"
# Resolve NUB to an absolute path: every run happens in a subshell that cd's into
# a fixture dir, so a relative NUB= would fail to resolve there.
case "$NUB" in
  /*) ;;
  */*) NUB="$(cd "$(dirname "$NUB")" 2>/dev/null && pwd)/$(basename "$NUB")" ;;
  *) NUB="$(command -v "$NUB" 2>/dev/null || true)" ;;
esac
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"

# Defaults are screenshot-friendly. Increase these for publication-grade runs.
WARMUP=10
RUNS=30
PREWARM=10
MAX_LOAD=999   # pass --max-load 2 for a quiet-box publication run
FIXTURE_FILTER="true"
TOOLS="core"
SAVE_RESULTS=0
QUIET=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --warmup)        WARMUP="$2";          shift 2 ;;
    --runs)          RUNS="$2";            shift 2 ;;
    --prewarm)       PREWARM="$2";         shift 2 ;;
    --max-load)      MAX_LOAD="$2";        shift 2 ;;
    --fixture)       FIXTURE_FILTER="$2";  shift 2 ;;
    --tools)         TOOLS="$2";           shift 2 ;;
    --save)          SAVE_RESULTS=1;       shift ;;
    --quiet|--clean) QUIET=1;             shift ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done
case "$FIXTURE_FILTER" in true|node-e|both) ;; *) echo "ERROR: --fixture must be true, node-e, or both" >&2; exit 1 ;; esac
case "$TOOLS" in core|all) ;; *) echo "ERROR: --tools must be core or all" >&2; exit 1 ;; esac
if [[ "$SAVE_RESULTS" -eq 1 ]]; then
  RESULTS_DIR="$REPO_ROOT/tests/bench/script-runner/results"
else
  RESULTS_DIR="$(mktemp -d /tmp/nub-bench-results-XXXXXX)"
fi

[[ -x "$NUB" ]] || { echo "ERROR: nub not found at $NUB" >&2; exit 1; }
command -v hyperfine &>/dev/null || { echo "ERROR: hyperfine not found." >&2; exit 1; }
command -v node &>/dev/null      || { echo "ERROR: node not found." >&2; exit 1; }
command -v npm  &>/dev/null      || { echo "ERROR: npm not found." >&2; exit 1; }
command -v pnpm &>/dev/null      || { echo "ERROR: pnpm not found." >&2; exit 1; }

# `node --run` landed in Node 22. Hard-fail with a clear message if absent.
node --run >/dev/null 2>&1 || true   # `node --run` with no arg prints usage (exit !=0); presence-check below
if ! node --help 2>&1 | grep -q -- '--run'; then
  echo "ERROR: this node ($(node --version)) does not support 'node --run' (needs >=22)." >&2
  exit 1
fi

# --- Idle-machine discipline -------------------------------------------------
one_min_load() { uptime | sed -E 's/.*load averages?: *([0-9.]+).*/\1/'; }
WAITED=0
while :; do
  L="$(one_min_load)"
  awk -v l="$L" -v m="$MAX_LOAD" 'BEGIN{exit !(l<m)}' && break
  if [[ $WAITED -ge 120 ]]; then
    echo "WARNING: 1-min load=$L still >= $MAX_LOAD after ${WAITED}s; proceeding but flagging." >&2
    break
  fi
  echo "  [load=$L >= $MAX_LOAD — waiting 15s for the box to quiet down...]" >&2
  sleep 15; WAITED=$((WAITED+15))
done
LOAD_AT_RUN="$(one_min_load)"

# --- Fixtures ----------------------------------------------------------------
# Each is a no-dep project; the script body is what differs.
make_fixture() {  # $1 = body command string
  local d; d="$(mktemp -d /tmp/nub-vs-node-XXXXXX)"
  cat > "$d/package.json" <<EOF
{
  "name": "vs-node-bench",
  "version": "1.0.0",
  "scripts": {
    "noop": $(printf '%s' "$1" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read()))')
  }
}
EOF
  printf '%s' "$d"
}

FIX_TRUE="$(make_fixture 'true')"
FIX_NODE="$(make_fixture 'node -e ""')"
trap 'rm -rf "$FIX_TRUE" "$FIX_NODE"' EXIT

# --- Exit-0 verification: every command must actually RUN the script ---------
# `node --run` errors on a missing script with a DIFFERENT exit path, so we must
# confirm exit 0 (script genuinely dispatched), not merely "ran without crash".
verify() {  # $1=label $2=dir $3...=command
  local label="$1" dir="$2"; shift 2
  if ( cd "$dir" && "$@" >/dev/null 2>&1 ); then
    if [[ "$QUIET" -eq 0 ]]; then echo "  [verify] $label → exit 0 ✓"; fi
  else
    echo "ERROR: '$label' did not exit 0 in $dir (got $?). Aborting — refuse to time an error path." >&2
    exit 1
  fi
}

if [[ "$QUIET" -eq 0 ]]; then
  echo "================================================================"
  echo "  nub run  vs  node --run  — script DISPATCH benchmark"
  echo "  nub:  $("$NUB" --version 2>&1 | head -1)"
  echo "  node: $(node --version)   npm: $(npm --version)   pnpm: $(pnpm --version)"
  echo "  tools: $TOOLS  prewarm: $PREWARM  hyperfine warmup: $WARMUP  runs: $RUNS  save: $([[ $SAVE_RESULTS -eq 1 ]] && echo yes || echo no)"
  echo "  date: $(date)"
  echo "  load@run (1-min): $LOAD_AT_RUN   full uptime: $(uptime)"
  echo "================================================================"
  echo "  Exit-0 verification (each command must dispatch the script):"
fi
verify_fixture() {
  local fix="$1"
  verify "nub run"    "$fix" "$NUB" run noop
  verify "node --run" "$fix" node --run noop
  if [[ "$TOOLS" == "all" ]]; then
    verify "npm run"  "$fix" npm run noop
    verify "pnpm run" "$fix" pnpm run noop
  fi
}
case "$FIXTURE_FILTER" in
  true)   verify_fixture "$FIX_TRUE" ;;
  node-e) verify_fixture "$FIX_NODE" ;;
  both)
    verify_fixture "$FIX_TRUE"
    verify_fixture "$FIX_NODE"
    ;;
esac
if [[ "$QUIET" -eq 0 ]]; then echo "================================================================"; fi

mkdir -p "$RESULTS_DIR"

run_fixture() {  # $1=tag $2=dir
  local tag="$1" dir="$2"
  local out="$RESULTS_DIR/script-runner-vs-node-${tag}-${TIMESTAMP}.json"
  if [[ "$QUIET" -eq 0 ]]; then
    echo ""
    echo "---- fixture: $tag  (script body: $(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["scripts"]["noop"])' "$dir/package.json")) ----"
  fi
  if [[ "$PREWARM" -gt 0 ]]; then
    if [[ "$QUIET" -eq 0 ]]; then echo "  [prewarm] running each command $PREWARM times before hyperfine..."; fi
    for _ in $(seq 1 "$PREWARM"); do
      ( cd "$dir" && "$NUB" run noop >/dev/null 2>&1 )
      ( cd "$dir" && node --run noop >/dev/null 2>&1 )
      if [[ "$TOOLS" == "all" ]]; then
        ( cd "$dir" && npm run noop >/dev/null 2>&1 )
        ( cd "$dir" && pnpm run noop >/dev/null 2>&1 )
      fi
    done
  fi
  local hyperfine_args=(
    --warmup "$WARMUP" --runs "$RUNS" --export-json "$out"
    --command-name "nub run" "cd '$dir' && '$NUB' run noop"
    --command-name "node --run" "cd '$dir' && node --run noop"
  )
  if [[ "$TOOLS" == "all" ]]; then
    hyperfine_args+=(
      --command-name "npm run" "cd '$dir' && npm run noop"
      --command-name "pnpm run" "cd '$dir' && pnpm run noop"
    )
  fi
  hyperfine "${hyperfine_args[@]}"
  if [[ "$QUIET" -eq 0 ]]; then
    echo "  [results saved → $out]"
  elif [[ "$SAVE_RESULTS" -eq 1 ]]; then
    echo "  [saved → $(basename "$out")]"
  fi
}

case "$FIXTURE_FILTER" in
  true)   run_fixture "true" "$FIX_TRUE" ;;
  node-e) run_fixture "node-e" "$FIX_NODE" ;;
  both)
    run_fixture "true" "$FIX_TRUE"
    run_fixture "node-e" "$FIX_NODE"
    ;;
esac

if [[ "$QUIET" -eq 0 ]]; then
  echo ""
  echo "================================================================"
  echo "  Results in $RESULTS_DIR/script-runner-vs-node-*-${TIMESTAMP}.json"
  echo "  load@run was: $LOAD_AT_RUN"
  echo "================================================================"
fi
