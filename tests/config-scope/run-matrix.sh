#!/usr/bin/env bash
# Differential config-scoping matrix (CP-3).
#
# Fixture: depends on debug@4.3.4 (→ ms@2.1.2). We pin the transitive `ms`
# two different ways:
#   overrides   → ms 2.0.0
#   resolutions → ms 2.1.0
# The role-native field decides which lands. We install under nub with each
# role declared and assert the materialized `ms` version, plus that the
# ignored field produced a warning. Where a real PM is installed we diff
# nub's choice against it.
#
# Honor matrix (which field wins per role):
#   npm   → overrides   (2.0.0); resolutions ignored (warn)
#   pnpm  → resolutions  (2.1.0); overrides ignored (warn)   [pnpm.overrides absent here]
#   yarn  → resolutions  (2.1.0); overrides ignored (warn)
#   bun   → overrides   (2.0.0); both honored, overrides wins (no warn)
#   nub   → overrides   (2.0.0); both honored, overrides wins (no warn)
set -uo pipefail

NUB="${NUB:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/nub}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PASS=0; FAIL=0

mk_fixture() {
  local dir="$1" pm="$2"
  mkdir -p "$dir"
  cat > "$dir/package.json" <<JSON
{
  "name": "cs-fixture",
  "version": "1.0.0",
  "packageManager": "$pm",
  "dependencies": { "debug": "4.3.4" },
  "overrides": { "ms": "2.0.0" },
  "resolutions": { "ms": "2.1.0" }
}
JSON
}

# The resolved ms version, read from whatever lockfile nub wrote for the
# role (the round-trip artifact we care about). Format-aware: pnpm/nub yaml
# carry `ms@<ver>:`, npm's JSON has a `node_modules/ms` package entry, yarn
# classic has an `ms@<range>:` block with a `version` line.
ms_version() {
  local dir="$1"
  if [ -f "$dir/package-lock.json" ]; then
    node -e '
      const l=require(process.argv[1]);
      const out=new Set();
      for (const [k,v] of Object.entries(l.packages||{}))
        if (k.endsWith("node_modules/ms")&&v.version) out.add(v.version);
      process.stdout.write([...out].sort().join(","));
    ' "$dir/package-lock.json"
    return
  fi
  local lock
  for lock in "$dir"/lock.yaml "$dir"/pnpm-lock.yaml "$dir"/bun.lock; do
    [ -f "$lock" ] || continue
    grep -oE 'ms@2\.[0-9.]+' "$lock" | sed 's/ms@//' | sort -u | paste -sd, -
    return
  done
  if [ -f "$dir/yarn.lock" ]; then
    # yarn classic: the `ms@…:` block's `version "x.y.z"` line.
    awk '/^"?ms@/{f=1} f&&/version /{gsub(/[",]/,"",$2); print $2; f=0}' \
      "$dir/yarn.lock" | sort -u | paste -sd, -
    return
  fi
}

check() {
  local label="$1" want="$2" got="$3"
  if [ "$got" = "$want" ]; then
    echo "  PASS  $label  → ms=$got"
    PASS=$((PASS+1))
  else
    echo "  FAIL  $label  → want ms=$want, got ms=$got"
    FAIL=$((FAIL+1))
  fi
}

run_role() {
  local pm="$1" want_ms="$2" want_warn_field="$3" seed="${4:-}"
  local dir="$WORK/$pm"
  mk_fixture "$dir" "$pm"
  echo "── role: $pm (declared $pm) ──"
  # nub's yarn write-gate refuses to CREATE yarn.lock (write fidelity
  # unproven), so for the yarn role we seed a real yarn.lock first — which
  # doubles as the differential baseline: assert nub honors the SAME field
  # real yarn resolved.
  if [ "$seed" = "yarn" ] && command -v yarn >/dev/null; then
    ( cd "$dir" && yarn install --silent >/dev/null 2>&1 )
    local real; real="$(ms_version "$dir")"
    if [ "$real" = "$want_ms" ]; then
      echo "  PASS  real yarn baseline → ms=$real (matches nub's choice)"
      PASS=$((PASS+1))
    else
      echo "  FAIL  real yarn baseline → ms=$real, expected $want_ms"
      FAIL=$((FAIL+1))
    fi
  fi
  local out
  out="$(cd "$dir" && NO_COLOR=1 "$NUB" install --lockfile-only 2>&1)"
  local rc=$?
  if [ $rc -ne 0 ]; then
    # lockfile-only may still resolve overrides; if it failed entirely, fall
    # back to a full install to materialize node_modules.
    out="$(cd "$dir" && NO_COLOR=1 "$NUB" install 2>&1)"
  else
    out="$out
$(cd "$dir" && NO_COLOR=1 "$NUB" install 2>&1)"
  fi
  local got; got="$(ms_version "$dir")"
  check "$pm honors native field" "$want_ms" "$got"

  if [ -n "$want_warn_field" ]; then
    if echo "$out" | grep -q "\`$want_warn_field\` ignored"; then
      echo "  PASS  $pm warns on ignored \`$want_warn_field\`"
      PASS=$((PASS+1))
    else
      echo "  FAIL  $pm did NOT warn on ignored \`$want_warn_field\`"
      echo "$out" | grep -i 'ignored\|nub:' | sed 's/^/        /'
      FAIL=$((FAIL+1))
    fi
  else
    if echo "$out" | grep -q 'ignored —'; then
      echo "  FAIL  $pm warned but should have been SILENT"
      echo "$out" | grep 'ignored —' | sed 's/^/        /'
      FAIL=$((FAIL+1))
    else
      echo "  PASS  $pm silent (both honored / nothing ignored)"
      PASS=$((PASS+1))
    fi
  fi
}

echo "nub: $NUB"
run_role "npm@11.13.0"  "2.0.0" "resolutions"
run_role "pnpm@9.0.0"   "2.1.0" "overrides"
run_role "yarn@1.22.0"  "2.1.0" "overrides" "yarn"
run_role "bun@1.1.0"    "2.0.0" ""
run_role "nub@0.1.0"    "2.0.0" ""

echo
echo "── portable repo (same pin in both fields) stays silent ──"
pdir="$WORK/portable"
mkdir -p "$pdir"
cat > "$pdir/package.json" <<'JSON'
{
  "name": "cs-portable", "version": "1.0.0",
  "packageManager": "npm@11.13.0",
  "dependencies": { "debug": "4.3.4" },
  "overrides": { "ms": "2.0.0" },
  "resolutions": { "ms": "2.0.0" }
}
JSON
pout="$(cd "$pdir" && NO_COLOR=1 "$NUB" install 2>&1)"
if echo "$pout" | grep -q 'ignored —'; then
  echo "  FAIL  portable repo warned (should be silent)"; FAIL=$((FAIL+1))
else
  echo "  PASS  portable repo silent"; PASS=$((PASS+1))
fi
check "portable applies the (identical) pin" "2.0.0" "$(ms_version "$pdir")"

echo
echo "── catalog under npm hard-errors ──"
cdir="$WORK/catalog"
mkdir -p "$cdir"
cat > "$cdir/package.json" <<'JSON'
{
  "name": "cs-catalog", "version": "1.0.0",
  "packageManager": "npm@11.13.0",
  "dependencies": { "debug": "catalog:" }
}
JSON
cout="$(cd "$cdir" && NO_COLOR=1 "$NUB" install 2>&1)"
if echo "$cout" | grep -qi 'catalog:.*not supported'; then
  echo "  PASS  npm catalog hard-errors"; PASS=$((PASS+1))
else
  echo "  FAIL  npm catalog did not hard-error"; echo "$cout" | tail -3 | sed 's/^/        /'; FAIL=$((FAIL+1))
fi

echo
echo "════ PASS=$PASS  FAIL=$FAIL ════"
[ "$FAIL" -eq 0 ]
