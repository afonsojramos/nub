#!/usr/bin/env bash
# Regression harness for the GVS @parcel/core store-dir over-split.
#
# For each Parcel version: fresh fixture + fresh isolated global store
# (XDG_CACHE_HOME/XDG_DATA_HOME), install with the global virtual store
# forced on, then assert:
#   1. exactly ONE @parcel/core store dir (no over-split), and
#   2. `parcel build` exits 0 (no worker-farm DataCloneError).
#
# The shared CAS is isolated per run so results never depend on a
# polluted machine-global store — the accumulation trap that masked the
# original root-cause. See README.md for the mechanism.
#
# Usage: run.sh <nub-binary> [version ...]
#   default versions: 2.9.3 2.10.3 2.11.0 2.12.0 2.13.3 2.16.4
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
NUB="${1:?usage: run.sh <nub-binary> [version ...]}"
shift || true
VERSIONS=("$@")
[ ${#VERSIONS[@]} -eq 0 ] && VERSIONS=(2.9.3 2.10.3 2.11.0 2.12.0 2.13.3 2.16.4)

fail=0
printf "%-10s %-8s %-6s %-8s %s\n" VERSION INSTALL CORES BUILD RESULT
for V in "${VERSIONS[@]}"; do
  DEST=$(mktemp -d "/tmp/nub-parcel-gvs-$V-XXXXXX")
  ISO=$(mktemp -d "/tmp/nub-parcel-gvs-iso-$V-XXXXXX")
  "$HERE/make-fixture.sh" "$DEST" "$V" >/dev/null
  ( cd "$DEST"
    XDG_CACHE_HOME="$ISO/cache" XDG_DATA_HOME="$ISO/data" \
      NPM_CONFIG_ENABLE_GLOBAL_VIRTUAL_STORE=true "$NUB" install >install.log 2>&1
  ); IEXIT=$?
  VS="$ISO/cache/nub/pm/virtual-store"
  CORES=$(ls -1 "$VS" 2>/dev/null | grep -cE '^@parcel\+core@' || true)
  ( cd "$DEST"
    XDG_CACHE_HOME="$ISO/cache" XDG_DATA_HOME="$ISO/data" "$NUB" run build >build.log 2>&1
  ); BEXIT=$?
  RESULT=PASS
  if [ "$IEXIT" -ne 0 ] || [ "$BEXIT" -ne 0 ] || [ "$CORES" != 1 ]; then RESULT=FAIL; fail=1; fi
  printf "%-10s %-8s %-6s %-8s %s\n" "$V" \
    "$([ $IEXIT -eq 0 ] && echo ok || echo ERR)" "$CORES" \
    "$([ $BEXIT -eq 0 ] && echo ok || echo ERR)" "$RESULT"
  [ "$RESULT" = PASS ] && rm -rf "$DEST" "$ISO"
done
[ $fail -eq 0 ] && echo "ALL PASS" || { echo "FAILURES — kept failing fixtures under /tmp for inspection"; exit 1; }
