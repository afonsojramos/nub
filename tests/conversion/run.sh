#!/usr/bin/env bash
# Cross-format lockfile conversion harness — proves `nub pm use <target>` produces
# a lockfile the real target PM accepts frozen.
#
# MATRIX:
#   source PMs:  npm, pnpm, bun, yarn
#   target PMs:  npm, pnpm, bun  (yarn is write-refused — tested as a special leg)
#   skip source==target (no conversion needed)
#
# For each (source, target) pair the harness:
#   1. Generates a real lockfile with the SOURCE pm on a clean fixture copy.
#   2. Runs `nub pm use <target>[@<pin>]` to convert the lockfile.
#   3. Wipes node_modules and runs the TARGET pm's frozen-install.
#      The real PM is the honest judge — if it accepts the file, the conversion works.
#   4. Asserts every direct dep exists in node_modules.
#
# yarn-as-target converts the source lockfile into a classic (v1) yarn.lock and
# checks real yarn frozen-accepts it unchanged — the classic writer is proven
# against real yarn, so this is the normal convert→frozen-accept leg (the old
# "must refuse" contract was lifted). yarn→yarn keeps the existing yarn.lock.
#
# Usage:  run.sh [<path-to-nub>] [fixture ...]
# Env:
#   SANDBOX_ROOT=<dir>    reuse/inspect the sandbox (implies KEEP)
#   KEEP=1                keep the sandbox on success
#   SKIP_YARN=1           skip yarn legs even if yarn is on PATH
#   SKIP_BUN=1            skip bun legs even if bun is on PATH
#   PNPM_PIN=<ver>        pnpm version to pin in `nub pm use pnpm@<ver>`
#                         (defaults to installed pnpm version, or 10.15.1)
#   NPM_PIN=<ver>         npm version for `nub pm use npm@<ver>` (defaults to installed)
#   BUN_PIN=<ver>         bun version for `nub pm use bun@<ver>` (defaults to installed)
#
# Exit: 0 = all runnable legs pass; 1 = at least one FAIL.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NUB="${1:-}"
if [ -z "$NUB" ]; then
  for candidate in \
    "$(cd "$HERE/../.." && pwd)/target/release/nub" \
    "$(cd "$HERE/../.." && pwd)/target/debug/nub"; do
    [ -x "$candidate" ] && { NUB="$candidate"; break; }
  done
fi
shift 2>/dev/null || true
NUB="$(cd "$(dirname "$NUB")" && pwd)/$(basename "$NUB")"
[ -x "$NUB" ] || { echo "error: nub binary not found/executable: $NUB" >&2; exit 2; }

NUB_VERSION="$("$NUB" --version 2>/dev/null || echo '?')"

ALL_FIXTURES=(simple peers)
FIXTURES=("$@")
[ ${#FIXTURES[@]} -gt 0 ] || FIXTURES=("${ALL_FIXTURES[@]}")

# Detect available PMs.
HAVE_NPM=0;  command -v npm  >/dev/null 2>&1 && HAVE_NPM=1
HAVE_PNPM=0; command -v pnpm >/dev/null 2>&1 && HAVE_PNPM=1
HAVE_YARN=0; command -v yarn >/dev/null 2>&1 && [ "${SKIP_YARN:-0}" != "1" ] && HAVE_YARN=1
HAVE_BUN=0;  command -v bun  >/dev/null 2>&1 && [ "${SKIP_BUN:-0}"  != "1" ] && HAVE_BUN=1

NPM_VERSION="$(npm   --version 2>/dev/null || echo MISSING)"
PNPM_VERSION="$(pnpm --version 2>/dev/null || echo MISSING)"
YARN_VERSION="$(yarn --version 2>/dev/null || echo MISSING)"
BUN_VERSION="$(bun   --version 2>/dev/null || echo MISSING)"

# PM pins for `nub pm use <pm>@<pin>` — pin to the installed version so the
# converted lockfile's packageManager declaration matches what's on PATH.
PNPM_PIN="${PNPM_PIN:-${PNPM_VERSION:-10.15.1}}"
NPM_PIN="${NPM_PIN:-${NPM_VERSION:-11.13.0}}"
BUN_PIN="${BUN_PIN:-${BUN_VERSION:-1.3.14}}"

# Hermetic sandbox.
CREATED_SANDBOX=0
if [ -z "${SANDBOX_ROOT:-}" ]; then
  SANDBOX_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/nub-conversion.XXXXXX")"
  CREATED_SANDBOX=1
fi
mkdir -p "$SANDBOX_ROOT/home" "$SANDBOX_ROOT/runs" "$SANDBOX_ROOT/logs"
export HOME="$SANDBOX_ROOT/home"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"

# Clear any PM env that could steer lockfile format decisions.
unset npm_config_default_lockfile_format NPM_CONFIG_DEFAULT_LOCKFILE_FORMAT 2>/dev/null || true

echo "=== nub cross-format lockfile conversion harness ==="
echo "nub:      $NUB ($NUB_VERSION)"
echo "npm:      $NPM_VERSION  (HAVE=$HAVE_NPM, pin=$NPM_PIN)"
echo "pnpm:     $PNPM_VERSION  (HAVE=$HAVE_PNPM, pin=$PNPM_PIN)"
echo "yarn:     $YARN_VERSION  (HAVE=$HAVE_YARN)"
echo "bun:      $BUN_VERSION  (HAVE=$HAVE_BUN, pin=$BUN_PIN)"
echo "sandbox:  $SANDBOX_ROOT"
echo ""

step() {
  local log="$1" label="$2"; shift 2
  { echo; echo "### $label"; echo "### \$ $*"; } >>"$log"
  "$@" >>"$log" 2>&1
}

wipe_node_modules() {
  find "$1" -name node_modules -type d -prune -exec rm -rf {} +
}

stage_fixture() {
  local fixture="$1" proj="$2"
  rm -rf "$proj"
  mkdir -p "$proj"
  cp -R "$HERE/fixtures/$fixture/." "$proj/"
}

# assert_node_modules <proj> <log>
assert_node_modules() {
  local proj="$1" log="$2"
  local pkg="$proj/package.json"
  local failed=0
  local deps
  deps=$(node -e "
    const p = require('$pkg');
    const all = Object.keys({...p.dependencies, ...p.devDependencies});
    all.forEach(d => console.log(d));
  " 2>/dev/null) || { echo "FAILED: could not parse package.json" >>"$log"; return 1; }
  while IFS= read -r dep; do
    [ -z "$dep" ] && continue
    if [ ! -d "$proj/node_modules/$dep" ]; then
      echo "FAILED: node_modules/$dep missing after frozen install" >>"$log"
      failed=1
    fi
  done <<< "$deps"
  return $failed
}

# expected_reason <fixture> <src_pm> <tgt_pm> — look up a known-red conversion.
# Lines in expected-failures.txt: "<fixture> <src> <tgt> <reason...>".
# Mirrors the discipline in tests/conformance/expected-failures.txt — the list
# must SHRINK: a listed leg that now passes is reported XPASS-STALE and fails
# the run, so the green flip is recorded by deleting the entry in the same
# commit as the fix.
expected_reason() {
  awk -v f="$1" -v s="$2" -v t="$3" \
    '$1==f && $2==s && $3==t { $1=""; $2=""; $3=""; sub(/^  */,""); print; exit }' \
    "$HERE/expected-failures.txt" 2>/dev/null
}

RESULTS=()
FAILS=0
XPASSES=0

# leg <fixture> <source_pm> <target_pm> <proj> <log>
# Runs one conversion leg: source PM installs → nub converts → target PM frozen-installs.
leg() {
  local fixture="$1" src_pm="$2" tgt_pm="$3" proj="$4" log="$5"

  # ── Step 1: source PM writes its lockfile ─────────────────────────────────
  case "$src_pm" in
    npm)
      step "$log" "npm install (write lockfile)" \
        npm install --prefix "$proj" \
        || { echo "FAILED: npm install failed" >>"$log"; return 1; }
      [ -f "$proj/package-lock.json" ] \
        || { echo "FAILED: no package-lock.json written" >>"$log"; return 1; }
      ;;
    pnpm)
      ( cd "$proj" && step "$log" "pnpm install (write lockfile)" \
        pnpm install --no-frozen-lockfile ) \
        || { echo "FAILED: pnpm install failed" >>"$log"; return 1; }
      [ -f "$proj/pnpm-lock.yaml" ] \
        || { echo "FAILED: no pnpm-lock.yaml written" >>"$log"; return 1; }
      ;;
    bun)
      ( cd "$proj" && step "$log" "bun install (write lockfile)" \
        bun install ) \
        || { echo "FAILED: bun install failed" >>"$log"; return 1; }
      [ -f "$proj/bun.lock" ] \
        || { echo "FAILED: no bun.lock written" >>"$log"; return 1; }
      ;;
    yarn)
      ( cd "$proj" && step "$log" "yarn install (write lockfile)" \
        yarn install ) \
        || { echo "FAILED: yarn install failed" >>"$log"; return 1; }
      [ -f "$proj/yarn.lock" ] \
        || { echo "FAILED: no yarn.lock written" >>"$log"; return 1; }
      ;;
  esac

  # ── Step 2: nub pm use <target> converts the lockfile ─────────────────────
  local nub_pm_arg
  case "$tgt_pm" in
    npm)  nub_pm_arg="npm@$NPM_PIN"   ;;
    pnpm) nub_pm_arg="pnpm@$PNPM_PIN" ;;
    bun)  nub_pm_arg="bun@$BUN_PIN"   ;;
    yarn)
      # yarn-as-target: nub converts the source lockfile into a classic (v1)
      # yarn.lock (the classic writer is proven frozen-accepted by real yarn —
      # the old refusal gate was lifted). yarn→yarn keeps the existing file.
      local nub_exit=0
      ( cd "$proj" && step "$log" "nub pm use yarn" \
        "$NUB" pm use yarn ) >>"$log" 2>&1 || nub_exit=$?
      if [ "$nub_exit" -ne 0 ]; then
        echo "FAILED: $src_pm->yarn: nub pm use yarn exited $nub_exit" >>"$log"
        return 1
      fi
      [ -f "$proj/yarn.lock" ] \
        || { echo "FAILED: $src_pm->yarn: nub pm use yarn wrote no yarn.lock" >>"$log"; return 1; }
      cp "$proj/yarn.lock" "$log.converted-lock"
      wipe_node_modules "$proj"
      ( cd "$proj" && step "$log" "yarn install --frozen-lockfile (frozen accept)" \
        yarn install --frozen-lockfile --non-interactive ) \
        || { echo "FAILED: $src_pm->yarn: yarn rejected the converted yarn.lock (--frozen-lockfile)" >>"$log"; return 1; }
      cmp -s "$log.converted-lock" "$proj/yarn.lock" \
        || { echo "FAILED: $src_pm->yarn: yarn rewrote the converted yarn.lock (churn)" >>"$log"; return 1; }
      assert_node_modules "$proj" "$log" || return 1
      return 0
      ;;
  esac

  local nub_exit=0
  ( cd "$proj" && step "$log" "nub pm use $nub_pm_arg" \
    "$NUB" pm use "$nub_pm_arg" ) || nub_exit=$?
  if [ "$nub_exit" -ne 0 ]; then
    echo "FAILED: nub pm use $nub_pm_arg exited $nub_exit" >>"$log"
    return 1
  fi

  # Confirm target lockfile exists.
  local target_lockfile
  case "$tgt_pm" in
    npm)  target_lockfile="$proj/package-lock.json" ;;
    pnpm) target_lockfile="$proj/pnpm-lock.yaml"    ;;
    bun)  target_lockfile="$proj/bun.lock"          ;;
  esac
  [ -f "$target_lockfile" ] \
    || { echo "FAILED: nub pm use $nub_pm_arg wrote no $tgt_pm lockfile at $target_lockfile" >>"$log"; return 1; }

  # Capture converted lockfile for diff on failure.
  cp "$target_lockfile" "$log.converted-lock"

  # ── Step 3: wipe node_modules, target PM frozen-install ───────────────────
  wipe_node_modules "$proj"

  case "$tgt_pm" in
    npm)
      ( cd "$proj" && step "$log" "npm ci (frozen accept)" \
        npm ci ) \
        || { echo "FAILED: npm ci rejected the converted lockfile" >>"$log"; return 1; }
      ;;
    pnpm)
      ( cd "$proj" && step "$log" "pnpm install --frozen-lockfile (frozen accept)" \
        pnpm install --frozen-lockfile ) \
        || { echo "FAILED: pnpm install --frozen-lockfile rejected the converted lockfile" >>"$log"; return 1; }
      ;;
    bun)
      ( cd "$proj" && step "$log" "bun install --frozen-lockfile (frozen accept)" \
        bun install --frozen-lockfile ) \
        || { echo "FAILED: bun install --frozen-lockfile rejected the converted lockfile" >>"$log"; return 1; }
      ;;
  esac

  # ── Step 4: assert node_modules correctness ───────────────────────────────
  assert_node_modules "$proj" "$log" || return 1

  return 0
}

for fixture in "${FIXTURES[@]}"; do
  [ -d "$HERE/fixtures/$fixture" ] \
    || { echo "error: unknown fixture '$fixture'" >&2; exit 2; }

  # Source PMs: all four (yarn only if available).
  declare -a src_pms=()
  [ "$HAVE_NPM"  -eq 1 ] && src_pms+=(npm)
  [ "$HAVE_PNPM" -eq 1 ] && src_pms+=(pnpm)
  [ "$HAVE_BUN"  -eq 1 ] && src_pms+=(bun)
  [ "$HAVE_YARN" -eq 1 ] && src_pms+=(yarn)

  # Target PMs: npm, pnpm, bun (real frozen judge) + yarn (refusal judge).
  declare -a tgt_pms=()
  [ "$HAVE_NPM"  -eq 1 ] && tgt_pms+=(npm)
  [ "$HAVE_PNPM" -eq 1 ] && tgt_pms+=(pnpm)
  [ "$HAVE_BUN"  -eq 1 ] && tgt_pms+=(bun)
  # yarn as target: always test if yarn is available — it's a refusal assertion.
  [ "$HAVE_YARN" -eq 1 ] && tgt_pms+=(yarn)

  for src_pm in "${src_pms[@]}"; do
    for tgt_pm in "${tgt_pms[@]}"; do
      # No need to convert same format — skip (not a conversion).
      # Exception: yarn→yarn exercises the "lockfile kept as-is" path.
      if [ "$src_pm" = "$tgt_pm" ] && [ "$src_pm" != "yarn" ]; then
        continue
      fi

      label="$fixture | $src_pm → $tgt_pm"
      echo "--- $label"

      proj="$SANDBOX_ROOT/runs/$fixture--${src_pm}-to-${tgt_pm}"
      log="$SANDBOX_ROOT/logs/$fixture--${src_pm}-to-${tgt_pm}.log"
      : >"$log"
      stage_fixture "$fixture" "$proj"

      ok=0
      leg "$fixture" "$src_pm" "$tgt_pm" "$proj" "$log" || ok=$?

      reason="$(expected_reason "$fixture" "$src_pm" "$tgt_pm")"
      if [ "$ok" -eq 0 ] && [ -z "$reason" ]; then
        echo "    PASS"
        RESULTS+=("$fixture|${src_pm}→${tgt_pm}|PASS|-")
      elif [ "$ok" -eq 0 ] && [ -n "$reason" ]; then
        # Stale expected-failure entry: fix landed without removing the entry.
        echo "    XPASS-STALE: now passes — remove from expected-failures.txt: $reason"
        XPASSES=$((XPASSES + 1))
        RESULTS+=("$fixture|${src_pm}→${tgt_pm}|XPASS-STALE|$reason")
      elif [ -n "$reason" ]; then
        echo "    expected red: $reason"
        RESULTS+=("$fixture|${src_pm}→${tgt_pm}|RED (expected)|$reason")
      else
        FAILS=$((FAILS + 1))
        echo "    FAIL — log: $log"
        tail -n 25 "$log" | sed 's/^/    | /'
        # On fail, also show the converted lockfile if it exists.
        if [ -f "$log.converted-lock" ]; then
          echo "    | --- converted lockfile ($tgt_pm) ---"
          head -n 30 "$log.converted-lock" | sed 's/^/    | /'
        fi
        # Capture error summary
        local_err="$(grep -E 'FAILED|Error:|error:' "$log" | head -3 | tr '\n' ';')"
        RESULTS+=("$fixture|${src_pm}→${tgt_pm}|FAIL|$local_err")
      fi
    done
  done
done

[ "$HAVE_NPM"  -eq 0 ] && echo "NOTE: npm not on PATH — npm legs skipped"
[ "$HAVE_PNPM" -eq 0 ] && echo "NOTE: pnpm not on PATH — pnpm legs skipped"
[ "$HAVE_YARN" -eq 0 ] && echo "NOTE: yarn not on PATH (or SKIP_YARN=1) — yarn legs skipped"
[ "$HAVE_BUN"  -eq 0 ] && echo "NOTE: bun not on PATH (or SKIP_BUN=1) — bun legs skipped"

echo ""
echo "=== results ==="
printf '%-12s %-20s %-6s %s\n' "fixture" "conversion" "result" "notes"
for row in "${RESULTS[@]}"; do
  IFS='|' read -r f conv status notes <<<"$row"
  printf '%-12s %-20s %-6s %s\n' "$f" "$conv" "$status" "$notes"
done
echo ""

if [ "$FAILS" -gt 0 ] || [ "$XPASSES" -gt 0 ]; then
  echo "RESULT: FAIL ($FAILS unexpected failure(s), $XPASSES stale expected-failure entry/entries)"
  echo "sandbox kept for forensics: $SANDBOX_ROOT"
  exit 1
fi

echo "RESULT: OK (expected reds, if any, are listed above and tracked in expected-failures.txt)"
if [ "$CREATED_SANDBOX" -eq 1 ] && [ "${KEEP:-0}" != "1" ]; then
  rm -rf "$SANDBOX_ROOT"
else
  echo "sandbox kept: $SANDBOX_ROOT"
fi
