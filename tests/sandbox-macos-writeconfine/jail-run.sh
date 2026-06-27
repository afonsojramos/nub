#!/usr/bin/env bash
# Run a lifecycle command under the macOS write-confine SBPL profile, with the
# npm lifecycle environment, capturing the exit code + any write-denial paths.
#
# Two passes are the experiment (see README.md):
#   strict  — pkg dir + tmp only (NO cache allowlist)
#   relaxed — + the cache allowlist (--write roots)
# plus a --control pass that runs UN-sandboxed to confirm the build works on this
# machine at all (so a sandbox FAIL is attributable to write-confine, not a
# broken package).
#
# Usage:
#   jail-run.sh --pkg <dir> [--project <dir>] [--write <dir>]... [--tmp <dir>] \
#               --mode strict|relaxed|control [--label NAME] -- <cmd> [args...]
#
# The command runs with cwd = the package dir and an npm-lifecycle-shaped env
# (npm_config_* build hints, INIT_CWD, PATH including the tree's node_modules/.bin).
#
# Output: prints the captured exit code and a de-duplicated list of denied write
# paths parsed from the child's combined output (build tools print the offending
# path on EPERM/EACCES). Also streams the macOS unified-log Sandbox violations for
# this run (no sudo needed for our own user's processes) when `log` is available.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$HERE/gen-profile.mjs"

PKG=""; PROJECT=""; MODE="strict"; LABEL=""
WRITES=(); TMPS=()
CMD=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pkg) PKG="$2"; shift 2;;
    --project) PROJECT="$2"; shift 2;;
    --write) WRITES+=("$2"); shift 2;;
    --tmp) TMPS+=("$2"); shift 2;;
    --mode) MODE="$2"; shift 2;;
    --label) LABEL="$2"; shift 2;;
    --) shift; CMD=("$@"); break;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
[[ -z "$PKG" ]] && { echo "jail-run: --pkg required" >&2; exit 2; }
[[ ${#CMD[@]} -eq 0 ]] && { echo "jail-run: missing -- <cmd>" >&2; exit 2; }
[[ -z "$PROJECT" ]] && PROJECT="$PKG"

# A private scratch tmp for the build, granted write + made the child's tmp root.
# REPOINTING TMPDIR/TMP/TEMP at this granted dir is load-bearing: many build tools
# (node-gyp's atomic header extract, cc/ld scratch) `mkdtemp` in `os.tmpdir()` =
# the OS temp ROOT (`/var/folders/.../T`), which is NOT in the allowlist. Pointing
# the tmp anchors at the granted private dir lands those writes in-allowlist — the
# `tmp: private` shorthand from the design (see results.md §Tmp anchor).
SANDBOX_TMP="${NUB_SANDBOX_TMP:-$(mktemp -d "${TMPDIR:-/tmp}/nub-jail.XXXXXX")}"
TMPS+=("$SANDBOX_TMP")
export TMPDIR="$SANDBOX_TMP" TMP="$SANDBOX_TMP" TEMP="$SANDBOX_TMP"

# Build the npm-lifecycle env. PATH carries the tree's hoisted .bin so transitive
# build tools (node-gyp, prebuild-install, node-pre-gyp) resolve.
BIN="$PROJECT/node_modules/.bin"
export PATH="$BIN:$PATH"
export INIT_CWD="$PROJECT"
export npm_config_cache="${npm_config_cache:-$SANDBOX_TMP/npm-cache}"
export npm_lifecycle_event="${npm_lifecycle_event:-install}"

LOG="$(mktemp "${TMPDIR:-/tmp}/jail-run.out.XXXXXX")"

run_label="${LABEL:-$(basename "$PKG")}"
echo "=== [$run_label] mode=$MODE pkg=$PKG ===" >&2

if [[ "$MODE" == "control" ]]; then
  ( cd "$PKG" && "${CMD[@]}" ) >"$LOG" 2>&1
  CODE=$?
else
  GENARGS=(--pkg "$PKG" --project "$PROJECT" --mode "$MODE" --darwin-temp)
  for w in "${WRITES[@]}"; do GENARGS+=(--write "$w"); done
  for t in "${TMPS[@]}"; do GENARGS+=(--tmp "$t"); done
  PROFILE="$(node "$GEN" "${GENARGS[@]}")" || { echo "profile-gen failed" >&2; exit 3; }
  ( cd "$PKG" && exec sandbox-exec -p "$PROFILE" -- "${CMD[@]}" ) >"$LOG" 2>&1
  CODE=$?
fi

echo "--- exit: $CODE ---"
echo "--- output (tail) ---"
tail -40 "$LOG"
echo "--- denied write paths (parsed) ---"
# Build tools print the offending path next to EPERM/EACCES/not permitted.
grep -oE '(/[^ :"'"'"']+)' "$LOG" \
  | grep -E '^/' \
  | sort -u \
  | grep -iE 'permission|denied' >/dev/null 2>&1 || true
# More reliable: pull lines mentioning the denial, then extract absolute paths.
grep -iE 'operation not permitted|EPERM|EACCES|permission denied|not permitted' "$LOG" \
  | grep -oE '/[^ :"'"'"',()]+' | sort -u || echo "(none parsed from child output)"
echo "RESULT $run_label mode=$MODE exit=$CODE"
echo "(full log: $LOG)"
