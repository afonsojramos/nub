#!/usr/bin/env bash
# Lockfile MUTATION differential harness — the write-path counterpart to the
# static round-trip in tests/conformance/. Every conformance fixture is a
# STATIC install; nothing exercised `nub add` / `nub remove` / `nub update`
# against a lockfile a real PM already wrote. That is where write-path bugs
# live (a static install can pass while an `add` churns or corrupts), and it is
# the single biggest coverage gap. See README.md for the full design.
#
# The loop, per (fixture, mutation, pm):
#
#   1. stage the fixture into two parallel copies: `nub/` and `ref/`.
#   2. the REAL PM installs in BOTH (identical pre-mutation baseline lockfile +
#      node_modules).
#   3. MUTATE: in `nub/`, run `nub <add|remove|update>`; in `ref/`, run the
#      EQUIVALENT real-PM mutation (`pnpm add` / `npm install` / `bun add` …).
#   4. assert (a) FROZEN-ACCEPT: the real PM frozen-installs nub's mutated
#      lockfile and does NOT rewrite it (a frozen install must be a no-op on a
#      well-formed lockfile — `cmp` byte-identity before/after the frozen run).
#   5. assert (b) SEMANTIC EQUIVALENCE: nub's mutated lockfile and the real PM's
#      mutated lockfile describe the same resolved graph (same direct-spec map +
#      same resolved-version multiset), ignoring ordering/formatting. This is
#      the differential — run the same mutation with the real PM on a parallel
#      copy and compare the SEMANTIC content, never the bytes (`add` ordering
#      legitimately differs run-to-run). extract-graph.mjs + compare-graphs.mjs.
#
# We compare nub-<pm> vs real-<pm> ALWAYS — never cross-PM (PMs legitimately
# resolve/dedup differently from each other; each nub-format must match ITS OWN
# reference PM).
#
# Conventions mirror tests/conformance/run.sh: hermetic HOME/XDG sandbox, pinned
# PMs on PATH, cold bun cache for honest integrity verification, a skip_reason()
# for ecosystem-level impossibilities, and an expected-failures.txt of known-red
# nub mutation bugs that must SHRINK.
#
# yarn is READ-ONLY in nub (no write-path mutation), so it is skipped entirely.
#
# Usage:  run-mutations.sh [<path-to-nub>] [fixture ...]
# Env:    SANDBOX_ROOT=<dir>   reuse/inspect the sandbox (implies KEEP)
#         KEEP=1               keep the sandbox on success
#         SKIP_BUN=1           skip bun legs even if bun is on PATH
# Exit:   0 = all required legs pass (skips for missing tools are fine);
#         1 = at least one unexpected FAIL or stale expected-failure entry.
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
NUB_VERSION="$("$NUB" --version 2>/dev/null | head -1 || echo '?')"

EXTRACT="$HERE/extract-graph.mjs"
COMPARE="$HERE/compare-graphs.mjs"
[ -f "$EXTRACT" ] && [ -f "$COMPARE" ] || { echo "error: extract/compare scripts missing in $HERE" >&2; exit 2; }

ALL_FIXTURES=(m1-add-noconflict m3-add-dedup m5-remove-prune)
FIXTURES=("$@")
[ ${#FIXTURES[@]} -gt 0 ] || FIXTURES=("${ALL_FIXTURES[@]}")

HAVE_NPM=0;  command -v npm  >/dev/null 2>&1 && HAVE_NPM=1
HAVE_PNPM=0; command -v pnpm >/dev/null 2>&1 && HAVE_PNPM=1
HAVE_BUN=0;  command -v bun  >/dev/null 2>&1 && [ "${SKIP_BUN:-0}" != "1" ] && HAVE_BUN=1

NPM_VERSION="$(npm  --version 2>/dev/null || echo MISSING)"
PNPM_VERSION="$(pnpm --version 2>/dev/null || echo MISSING)"
BUN_VERSION="$(bun  --version 2>/dev/null || echo MISSING)"

# Hermetic sandbox — redirect HOME + XDG so no dev-box config leaks in or out.
# Template deliberately avoids "aube" (brand sweep false-positive).
CREATED_SANDBOX=0
if [ -z "${SANDBOX_ROOT:-}" ]; then
  SANDBOX_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/nub-mutation.XXXXXX")"
  CREATED_SANDBOX=1
fi
mkdir -p "$SANDBOX_ROOT/home" "$SANDBOX_ROOT/runs" "$SANDBOX_ROOT/logs"
export HOME="$SANDBOX_ROOT/home"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
unset npm_config_default_lockfile_format NPM_CONFIG_DEFAULT_LOCKFILE_FORMAT 2>/dev/null || true

echo "=== nub lockfile-mutation differential ==="
echo "nub:      $NUB ($NUB_VERSION)"
echo "npm:      $NPM_VERSION  (HAVE=$HAVE_NPM)"
echo "pnpm:     $PNPM_VERSION  (HAVE=$HAVE_PNPM)"
echo "bun:      $BUN_VERSION  (HAVE=$HAVE_BUN)"
echo "sandbox:  $SANDBOX_ROOT"
echo ""

step() {
  local log="$1" label="$2"; shift 2
  { echo; echo "### $label"; echo "### \$ $*"; } >>"$log"
  "$@" >>"$log" 2>&1
}

wipe_node_modules() { find "$1" -name node_modules -type d -prune -exec rm -rf {} + 2>/dev/null || true; }

stage_fixture() {
  local fixture="$1" proj="$2"
  rm -rf "$proj"; mkdir -p "$proj"
  # Copy only package.json (and any extra manifest files) — NOT the `mutation`
  # spec file, which is harness metadata, not project content.
  cp "$HERE/fixtures/$fixture/package.json" "$proj/package.json"
  for extra in pnpm-workspace.yaml .npmrc; do
    [ -f "$HERE/fixtures/$fixture/$extra" ] && cp "$HERE/fixtures/$fixture/$extra" "$proj/$extra"
  done
}

# Read a `<verb>: <args>` line from the fixture's mutation spec.
mutation_field() {
  local fixture="$1" verb="$2"
  awk -F': *' -v v="$verb" '!/^#/ && $1==v { print $2; exit }' "$HERE/fixtures/$fixture/mutation"
}

# Per-PM real-PM install (writes lockfile + node_modules). Pre-mutation baseline.
real_install() {
  local pm="$1" proj="$2" log="$3"
  case "$pm" in
    npm)  ( cd "$proj" && step "$log" "npm install (baseline)"  npm install ) ;;
    pnpm) ( cd "$proj" && step "$log" "pnpm install (baseline)" pnpm install --no-frozen-lockfile ) ;;
    bun)  ( cd "$proj" && step "$log" "bun install (baseline)"  env BUN_INSTALL_CACHE_DIR="$proj/.bun-cache" bun install ) ;;
  esac
}

# Per-PM real-PM mutation matching nub's. `add: X` -> `<pm> add X`;
# `remove: X` -> `<pm> remove X`; `update: X` -> `<pm> update X`.
real_mutate() {
  local pm="$1" proj="$2" log="$3" verb="$4" args="$5"
  # shellcheck disable=SC2086
  case "$pm--$verb" in
    npm--add)     ( cd "$proj" && step "$log" "npm install $args"   npm install $args ) ;;
    npm--remove)  ( cd "$proj" && step "$log" "npm uninstall $args" npm uninstall $args ) ;;
    npm--update)  ( cd "$proj" && step "$log" "npm update $args"    npm update $args ) ;;
    pnpm--add)    ( cd "$proj" && step "$log" "pnpm add $args"      pnpm add $args ) ;;
    pnpm--remove) ( cd "$proj" && step "$log" "pnpm remove $args"   pnpm remove $args ) ;;
    pnpm--update) ( cd "$proj" && step "$log" "pnpm update $args"   pnpm update $args ) ;;
    bun--add)     ( cd "$proj" && step "$log" "bun add $args"       env BUN_INSTALL_CACHE_DIR="$proj/.bun-cache" bun add $args ) ;;
    bun--remove)  ( cd "$proj" && step "$log" "bun remove $args"    env BUN_INSTALL_CACHE_DIR="$proj/.bun-cache" bun remove $args ) ;;
    bun--update)  ( cd "$proj" && step "$log" "bun update $args"    env BUN_INSTALL_CACHE_DIR="$proj/.bun-cache" bun update $args ) ;;
    *) echo "FAILED: no real-PM mapping for $pm $verb" >>"$log"; return 1 ;;
  esac
}

# nub's mutation. nub auto-detects the lockfile format already on disk.
nub_mutate() {
  local proj="$1" log="$2" verb="$3" args="$4"
  # shellcheck disable=SC2086
  case "$verb" in
    add)    ( cd "$proj" && step "$log" "nub add $args"    "$NUB" add $args ) ;;
    remove) ( cd "$proj" && step "$log" "nub remove $args" "$NUB" remove $args ) ;;
    update) ( cd "$proj" && step "$log" "nub update $args" "$NUB" update $args ) ;;
    *) echo "FAILED: unknown nub verb $verb" >>"$log"; return 1 ;;
  esac
}

lockfile_of() {
  case "$1" in
    npm)  echo "package-lock.json" ;;
    pnpm) echo "pnpm-lock.yaml" ;;
    bun)  echo "bun.lock" ;;
  esac
}

# Real PM frozen-accept of nub's mutated lockfile, with zero further churn.
frozen_accept() {
  local pm="$1" proj="$2" log="$3"
  local lf; lf="$(lockfile_of "$pm")"
  [ -f "$proj/$lf" ] || { echo "FAILED: nub produced no $lf to frozen-accept" >>"$log"; return 1; }
  cp "$proj/$lf" "$log.frozen-before"
  wipe_node_modules "$proj"
  case "$pm" in
    npm)  ( cd "$proj" && step "$log" "npm ci (frozen-accept nub lock)" npm ci ) \
            || { echo "FAILED: npm ci rejected nub's mutated package-lock.json" >>"$log"; return 1; } ;;
    pnpm) ( cd "$proj" && step "$log" "pnpm install --frozen-lockfile" pnpm install --frozen-lockfile ) \
            || { echo "FAILED: pnpm rejected nub's mutated pnpm-lock.yaml (--frozen-lockfile)" >>"$log"; return 1; } ;;
    bun)  ( cd "$proj" && step "$log" "bun install --frozen-lockfile (cold cache)" \
              env BUN_INSTALL_CACHE_DIR="$proj/.bun-cold-cache" bun install --frozen-lockfile ) \
            || { echo "FAILED: bun rejected nub's mutated bun.lock (--frozen-lockfile)" >>"$log"; return 1; } ;;
  esac
  cmp -s "$log.frozen-before" "$proj/$lf" || {
    echo "FAILED: $pm rewrote nub's mutated $lf during frozen install (churn)" >>"$log"
    diff -u "$log.frozen-before" "$proj/$lf" >>"$log" 2>&1 || true
    return 1
  }
  return 0
}

# Semantic differential: nub's mutated graph ≡ real-PM's mutated graph.
semantic_equal() {
  local pm="$1" nub_proj="$2" ref_proj="$3" log="$4"
  local ga="$log.graph-nub.json" gb="$log.graph-ref.json"
  node "$EXTRACT" "$nub_proj" --format "$pm" >"$ga" 2>>"$log" \
    || { echo "FAILED: could not extract graph from nub's $pm lockfile" >>"$log"; return 1; }
  node "$EXTRACT" "$ref_proj" --format "$pm" >"$gb" 2>>"$log" \
    || { echo "FAILED: could not extract graph from real $pm lockfile" >>"$log"; return 1; }
  { echo; echo "### semantic compare (nub vs real $pm)"; } >>"$log"
  if node "$COMPARE" "$ga" "$gb" --label-a "nub-$pm" --label-b "real-$pm" >>"$log" 2>&1; then
    return 0
  fi
  echo "FAILED: nub's mutated $pm graph diverges from real $pm's (see compare output above)" >>"$log"
  return 1
}

# One (fixture, pm) leg — run the whole mutation loop.
run_leg() {
  local fixture="$1" pm="$2" log="$3"
  local base="$SANDBOX_ROOT/runs/$fixture--$pm"
  local nub_proj="$base/nub" ref_proj="$base/ref"
  stage_fixture "$fixture" "$nub_proj"
  stage_fixture "$fixture" "$ref_proj"

  local verb args
  for v in add remove update; do
    args="$(mutation_field "$fixture" "$v")"
    [ -n "$args" ] && { verb="$v"; break; }
  done
  [ -n "${verb:-}" ] || { echo "FAILED: fixture $fixture has no mutation spec" >>"$log"; return 1; }

  real_install "$pm" "$nub_proj" "$log" || { echo "FAILED: baseline install ($pm) in nub copy" >>"$log"; return 1; }
  real_install "$pm" "$ref_proj" "$log" || { echo "FAILED: baseline install ($pm) in ref copy" >>"$log"; return 1; }

  nub_mutate "$nub_proj" "$log" "$verb" "$args" \
    || { echo "FAILED: nub $verb $args errored" >>"$log"; return 1; }
  real_mutate "$pm" "$ref_proj" "$log" "$verb" "$args" \
    || { echo "FAILED: real $pm $verb $args errored" >>"$log"; return 1; }

  # (a) frozen-accept zero-churn, then (b) semantic equivalence.
  frozen_accept "$pm" "$nub_proj" "$log" || return 1
  semantic_equal "$pm" "$nub_proj" "$ref_proj" "$log" || return 1
  return 0
}

skip_reason() {
  # Hook for future PM-specific mutation cases (none yet). Mirrors conformance.
  local fixture="$1" pm="$2"
  :
}

expected_reason() {
  awk -v f="$1" -v p="$2" \
    '!/^#/ && $1==f && $2==p { $1=""; $2=""; sub(/^  */,""); print; exit }' \
    "$HERE/expected-failures.txt" 2>/dev/null
}

RESULTS=(); FAILS=0; XPASSES=0

for fixture in "${FIXTURES[@]}"; do
  [ -d "$HERE/fixtures/$fixture" ] || { echo "error: unknown fixture '$fixture'" >&2; exit 2; }

  declare -a pms=()
  [ "$HAVE_NPM"  -eq 1 ] && pms+=(npm)
  [ "$HAVE_PNPM" -eq 1 ] && pms+=(pnpm)
  [ "$HAVE_BUN"  -eq 1 ] && pms+=(bun)

  for pm in "${pms[@]}"; do
    case "$pm" in
      npm)  pmv="$NPM_VERSION"  ;;
      pnpm) pmv="$PNPM_VERSION" ;;
      bun)  pmv="$BUN_VERSION"  ;;
    esac
    label="$fixture × $pm@$pmv"
    echo "--- $label"

    skip="$(skip_reason "$fixture" "$pm")"
    if [ -n "$skip" ]; then
      echo "    skip (by design): $skip"
      RESULTS+=("$fixture|$pm|$pmv|SKIP (by design)")
      continue
    fi

    log="$SANDBOX_ROOT/logs/$fixture--$pm.log"; : >"$log"
    ok=0
    run_leg "$fixture" "$pm" "$log" || ok=$?

    reason="$(expected_reason "$fixture" "$pm")"
    if [ "$ok" -eq 0 ] && [ -z "$reason" ]; then
      echo "    PASS"
      RESULTS+=("$fixture|$pm|$pmv|PASS")
    elif [ "$ok" -eq 0 ] && [ -n "$reason" ]; then
      echo "    XPASS-STALE: now passes — remove from expected-failures.txt: $reason"
      XPASSES=$((XPASSES + 1))
      RESULTS+=("$fixture|$pm|$pmv|XPASS-STALE")
    elif [ -n "$reason" ]; then
      echo "    expected red: $reason"
      RESULTS+=("$fixture|$pm|$pmv|RED (expected)")
    else
      FAILS=$((FAILS + 1))
      echo "    FAIL — log: $log"
      tail -n 24 "$log" | sed 's/^/    | /'
      RESULTS+=("$fixture|$pm|$pmv|FAIL")
    fi
  done
done

[ "$HAVE_NPM"  -eq 0 ] && echo "NOTE: npm not on PATH — npm legs skipped"
[ "$HAVE_PNPM" -eq 0 ] && echo "NOTE: pnpm not on PATH — pnpm legs skipped"
[ "$HAVE_BUN"  -eq 0 ] && echo "NOTE: bun not on PATH (or SKIP_BUN=1) — bun legs skipped"

echo ""
echo "=== results ==="
printf '%-22s %-6s %-12s %s\n' "fixture" "pm" "pm-version" "result"
for row in "${RESULTS[@]}"; do
  IFS='|' read -r f p v s <<<"$row"
  printf '%-22s %-6s %-12s %s\n' "$f" "$p" "$v" "$s"
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
