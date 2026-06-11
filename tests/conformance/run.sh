#!/usr/bin/env bash
# Drop-in PM conformance harness — proves nub is a true drop-in package manager
# in both directions:
#
#   Direction A (nub READS others): real PM writes its lockfile → nub
#     frozen-installs from it successfully → node_modules are correct.
#
#   Direction B (others READ nub): nub writes the lockfile → real PM
#     frozen-installs from it without modification (zero churn).
#
# See README.md for the full loop and design rationale.
#
# Usage:  run.sh [<path-to-nub>] [fixture ...]
# Env:    SANDBOX_ROOT=<dir>    reuse/inspect the sandbox (implies KEEP)
#         KEEP=1                keep the sandbox on success
#         SKIP_YARN=1           skip yarn legs even if yarn is on PATH
#         SKIP_BUN=1            skip bun legs even if bun is on PATH
#
# Exit: 0 = all required legs pass (skips for missing tools are fine);
#       1 = at least one FAIL.
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

# Fixture list — each is a subdirectory of fixtures/
ALL_FIXTURES=(simple peers)
FIXTURES=("$@")
[ ${#FIXTURES[@]} -gt 0 ] || FIXTURES=("${ALL_FIXTURES[@]}")

# Detect available PMs — skip with a loud note if absent.
HAVE_NPM=0;  command -v npm  >/dev/null 2>&1 && HAVE_NPM=1
HAVE_PNPM=0; command -v pnpm >/dev/null 2>&1 && HAVE_PNPM=1
HAVE_YARN=0; command -v yarn >/dev/null 2>&1 && [ "${SKIP_YARN:-0}" != "1" ] && HAVE_YARN=1
HAVE_BUN=0;  command -v bun  >/dev/null 2>&1 && [ "${SKIP_BUN:-0}"  != "1" ] && HAVE_BUN=1

NPM_VERSION="$(npm  --version 2>/dev/null || echo MISSING)"
PNPM_VERSION="$(pnpm --version 2>/dev/null || echo MISSING)"
YARN_VERSION="$(yarn --version 2>/dev/null || echo MISSING)"
BUN_VERSION="$(bun  --version 2>/dev/null || echo MISSING)"

# Hermetic sandbox: redirect HOME + XDG so no dev-box config leaks in or out.
# Mktemp template deliberately avoids "aube" (brand sweep would false-positive).
CREATED_SANDBOX=0
if [ -z "${SANDBOX_ROOT:-}" ]; then
  SANDBOX_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/nub-dropinpm.XXXXXX")"
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

echo "=== nub drop-in PM conformance ==="
echo "nub:      $NUB ($NUB_VERSION)"
echo "npm:      $NPM_VERSION  (HAVE=$HAVE_NPM)"
echo "pnpm:     $PNPM_VERSION  (HAVE=$HAVE_PNPM)"
echo "yarn:     $YARN_VERSION  (HAVE=$HAVE_YARN)"
echo "bun:      $BUN_VERSION  (HAVE=$HAVE_BUN)"
echo "sandbox:  $SANDBOX_ROOT"
echo ""

# step <log> <label> <cmd...> — append output to log, return exit code.
# Must be called from inside a ( cd "$proj" && ... ) subshell.
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

# assert_node_modules <proj> <log> — every direct dep from package.json must exist
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

RESULTS=()
FAILS=0

# ── Direction A (PM → nub): real PM writes lockfile, nub frozen-installs ────
dir_a() {
  local fixture="$1" pm="$2" pm_version="$3" proj="$4" log="$5"

  case "$pm" in
    pnpm)
      ( cd "$proj" && step "$log" "pnpm install (write lockfile)" \
        pnpm install --no-frozen-lockfile ) \
        || { echo "FAILED: pnpm install failed" >>"$log"; return 1; }
      [ -f "$proj/pnpm-lock.yaml" ] || { echo "FAILED: no pnpm-lock.yaml written" >>"$log"; return 1; }
      ;;
    npm)
      ( cd "$proj" && step "$log" "npm install (write lockfile)" \
        npm install ) \
        || { echo "FAILED: npm install failed" >>"$log"; return 1; }
      [ -f "$proj/package-lock.json" ] || { echo "FAILED: no package-lock.json written" >>"$log"; return 1; }
      ;;
    yarn)
      # yarn --no-lockfile means "resolve fresh but still write a lockfile"
      ( cd "$proj" && step "$log" "yarn install (write lockfile)" \
        yarn install ) \
        || { echo "FAILED: yarn install failed" >>"$log"; return 1; }
      [ -f "$proj/yarn.lock" ] || { echo "FAILED: no yarn.lock written" >>"$log"; return 1; }
      ;;
    bun)
      ( cd "$proj" && step "$log" "bun install (write lockfile)" \
        bun install ) \
        || { echo "FAILED: bun install failed" >>"$log"; return 1; }
      [ -f "$proj/bun.lock" ] || { echo "FAILED: no bun.lock written" >>"$log"; return 1; }
      ;;
  esac

  # Wipe node_modules so the frozen install is a real test
  wipe_node_modules "$proj"

  # nub frozen install from the lockfile the real PM just wrote
  ( cd "$proj" && step "$log" "nub install --frozen-lockfile" \
    "$NUB" install --frozen-lockfile ) \
    || { echo "FAILED: nub install --frozen-lockfile failed" >>"$log"; return 1; }

  # Assert node_modules correctness
  assert_node_modules "$proj" "$log" || return 1

  return 0
}

# ── Direction B (nub → PM): nub writes lockfile, real PM frozen-installs ────
dir_b() {
  local fixture="$1" pm="$2" pm_version="$3" proj="$4" log="$5"
  local lockfile_format=""

  case "$pm" in
    pnpm) lockfile_format=pnpm ;;
    npm)  lockfile_format=npm  ;;
    bun)  lockfile_format=bun  ;;
    yarn)
      # yarn v1 lockfile: nub should refuse to write it (refusal IS the contract).
      # See aube-conformance/run.sh leg_yarn for the rationale — yarn write
      # fidelity is unproven in the engine, so refusal is what nub promises.
      printf '# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n# yarn lockfile v1\n' > "$proj/yarn.lock"
      cp "$proj/yarn.lock" "$log.yarn-before"
      local code=0
      ( cd "$proj" && "$NUB" add kleur@4.1.5 ) >>"$log" 2>&1 || code=$?
      if [ "$code" -ne 0 ]; then
        echo "PASS: nub correctly refused to mutate yarn.lock (exit $code)" >>"$log"
        # The lockfile must also be untouched
        cmp -s "$log.yarn-before" "$proj/yarn.lock" \
          || { echo "FAILED: nub modified yarn.lock despite refusing" >>"$log"; return 1; }
        return 0
      else
        echo "FAILED: nub add succeeded despite yarn.lock (write-gate did not fire)" >>"$log"
        return 1
      fi
      ;;
  esac

  # Have nub write the lockfile for this format
  ( cd "$proj" && \
    step "$log" "nub install (write $lockfile_format lockfile)" \
    env npm_config_default_lockfile_format="$lockfile_format" "$NUB" install \
  ) || { echo "FAILED: nub install failed" >>"$log"; return 1; }

  local lockfile=""
  case "$pm" in
    pnpm) lockfile="$proj/pnpm-lock.yaml" ;;
    npm)  lockfile="$proj/package-lock.json" ;;
    bun)  lockfile="$proj/bun.lock" ;;
  esac
  [ -f "$lockfile" ] || { echo "FAILED: nub wrote no $lockfile_format lockfile at $lockfile" >>"$log"; return 1; }

  # Capture lockfile before real PM touches it (zero-churn baseline)
  cp "$lockfile" "$log.lock-before"

  # Wipe node_modules
  wipe_node_modules "$proj"

  # Real PM frozen install
  case "$pm" in
    pnpm)
      ( cd "$proj" && step "$log" "pnpm frozen accept" \
        pnpm install --frozen-lockfile ) \
        || { echo "FAILED: pnpm rejected nub's lockfile (--frozen-lockfile)" >>"$log"; return 1; }
      ;;
    npm)
      ( cd "$proj" && step "$log" "npm ci accept" \
        npm ci ) \
        || { echo "FAILED: npm rejected nub's lockfile (ci)" >>"$log"; return 1; }
      ;;
    bun)
      ( cd "$proj" && step "$log" "bun frozen accept" \
        bun install --frozen-lockfile ) \
        || { echo "FAILED: bun rejected nub's lockfile (--frozen-lockfile)" >>"$log"; return 1; }
      ;;
  esac

  # Zero-churn check: a frozen install must not rewrite the lockfile.
  cmp -s "$log.lock-before" "$lockfile" || {
    echo "FAILED: $pm rewrote the lockfile after frozen install (churn)" >>"$log"
    diff -u "$log.lock-before" "$lockfile" >>"$log" || true
    return 1
  }

  return 0
}

for fixture in "${FIXTURES[@]}"; do
  [ -d "$HERE/fixtures/$fixture" ] || { echo "error: unknown fixture '$fixture'" >&2; exit 2; }

  for direction in A B; do
    declare -a pms=()
    [ "$HAVE_NPM"  -eq 1 ] && pms+=(npm)
    [ "$HAVE_PNPM" -eq 1 ] && pms+=(pnpm)
    [ "$HAVE_YARN" -eq 1 ] && pms+=(yarn)
    [ "$HAVE_BUN"  -eq 1 ] && pms+=(bun)

    for pm in "${pms[@]}"; do
      case "$pm" in
        npm)  pm_version="$NPM_VERSION"  ;;
        pnpm) pm_version="$PNPM_VERSION" ;;
        yarn) pm_version="$YARN_VERSION" ;;
        bun)  pm_version="$BUN_VERSION"  ;;
      esac

      label="$fixture × dir-$direction × $pm@$pm_version"
      echo "--- $label"

      proj="$SANDBOX_ROOT/runs/$fixture--$direction--$pm"
      log="$SANDBOX_ROOT/logs/$fixture--$direction--$pm.log"
      : >"$log"
      stage_fixture "$fixture" "$proj"

      ok=0
      if [ "$direction" = "A" ]; then
        dir_a "$fixture" "$pm" "$pm_version" "$proj" "$log" || ok=$?
      else
        dir_b "$fixture" "$pm" "$pm_version" "$proj" "$log" || ok=$?
      fi

      if [ "$ok" -eq 0 ]; then
        echo "    PASS"
        RESULTS+=("$fixture|dir-$direction|$pm|$pm_version|PASS")
      else
        FAILS=$((FAILS + 1))
        echo "    FAIL — log: $log"
        tail -n 20 "$log" | sed 's/^/    | /'
        RESULTS+=("$fixture|dir-$direction|$pm|$pm_version|FAIL")
      fi
    done
  done
done

# Report any PMs that were skipped
[ "$HAVE_NPM"  -eq 0 ] && echo "NOTE: npm not on PATH — npm legs skipped"
[ "$HAVE_PNPM" -eq 0 ] && echo "NOTE: pnpm not on PATH — pnpm legs skipped"
[ "$HAVE_YARN" -eq 0 ] && echo "NOTE: yarn not on PATH (or SKIP_YARN=1) — yarn legs skipped"
[ "$HAVE_BUN"  -eq 0 ] && echo "NOTE: bun not on PATH (or SKIP_BUN=1) — bun legs skipped"

echo ""
echo "=== results ==="
printf '%-10s %-8s %-6s %-12s %s\n' "fixture" "dir" "pm" "pm-version" "result"
for row in "${RESULTS[@]}"; do
  IFS='|' read -r f d p v s <<<"$row"
  printf '%-10s %-8s %-6s %-12s %s\n' "$f" "$d" "$p" "$v" "$s"
done
echo ""

if [ "$FAILS" -gt 0 ]; then
  echo "RESULT: FAIL ($FAILS failure(s))"
  echo "sandbox kept for forensics: $SANDBOX_ROOT"
  exit 1
fi

echo "RESULT: OK"
if [ "$CREATED_SANDBOX" -eq 1 ] && [ "${KEEP:-0}" != "1" ]; then
  rm -rf "$SANDBOX_ROOT"
else
  echo "sandbox kept: $SANDBOX_ROOT"
fi
