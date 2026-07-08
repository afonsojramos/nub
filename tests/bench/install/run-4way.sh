#!/usr/bin/env bash
# One clean warm-install table: nub v0.3 (GVS on) vs nub v0.2 (GVS off) vs bun vs
# pnpm vs npm — five bars, so the GVS before/after is visible in one table.
#
# Warm install = warm global store + frozen lockfile, node_modules wiped between
# runs (in hyperfine --prepare, excluded from timing), no network. This is the
# case the global store exists to make cheap. Each tool gets its own copy of the
# fixture so their node_modules/lockfiles never collide; each store is pre-warmed
# once (untimed) before the timed run. The two nub bars are the SAME current binary
# with GVS toggled via the CI env (env -u CI = on = the fast symlink-into-store
# relink; CI=1 = off = the per-project full materialize the v0.2 layout shipped).
# Both nub bars install from nub.lock (nub's native lockfile).
#
# Usage:
#   cargo build --release -p nub-cli
#   bash tests/bench/install/run-4way.sh
#   NUB=/path/to/nub bash tests/bench/install/run-4way.sh --fixture tanstack-start --runs 12 --warmup 3
#
# Requires: hyperfine, pnpm, bun, npm on PATH; a release nub (NUB= or target/release/nub).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
DEFAULT_NUB="$REPO_ROOT/target/release/nub"
NUB="${NUB:-$DEFAULT_NUB}"
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
command -v hyperfine >/dev/null || { echo "hyperfine not found (brew install hyperfine)" >&2; exit 1; }

# ── Staleness guard ──────────────────────────────────────────────────────────
# The bench once silently measured a v0.2.8 target/release/nub (dev builds
# target/fast; nobody local-built release for 6+ versions), producing bogus
# numbers. Never again: the nub binary's version MUST match the workspace. If it
# is the default target/release/nub, auto-build it (ergonomic — the bench just
# works); if it is an explicit NUB= override that mismatches, hard-error.
WS_VERSION="$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
nub_semver() { "$1" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1; }
ensure_fresh_nub() {
  local is_default=0
  [ "$NUB" = "$DEFAULT_NUB" ] && is_default=1
  if [ ! -x "$NUB" ]; then
    if [ "$is_default" -eq 1 ]; then
      echo "== nub binary missing at $NUB — building release (cargo build --release -p nub-cli) =="
      ( cd "$REPO_ROOT" && cargo build --release -p nub-cli ) || { echo "ERROR: release build failed" >&2; exit 1; }
    else
      echo "ERROR: nub not found at NUB=$NUB. Build it: cargo build --release -p nub-cli" >&2
      exit 1
    fi
  fi
  local nv; nv="$(nub_semver "$NUB")"
  if [ "$nv" != "$WS_VERSION" ]; then
    if [ "$is_default" -eq 1 ]; then
      echo "== STALE nub binary: $NUB is v$nv but workspace is v$WS_VERSION — rebuilding release =="
      ( cd "$REPO_ROOT" && cargo build --release -p nub-cli ) || { echo "ERROR: release build failed" >&2; exit 1; }
      nv="$(nub_semver "$NUB")"
      [ "$nv" = "$WS_VERSION" ] || { echo "ERROR: after rebuild nub is still v$nv, expected v$WS_VERSION" >&2; exit 1; }
    else
      echo "ERROR: stale/mismatched nub binary: NUB=$NUB is v$nv but workspace is v$WS_VERSION." >&2
      echo "       Rebuild release and re-run: cargo build --release -p nub-cli" >&2
      exit 1
    fi
  fi
  echo "== nub binary OK: v$nv (matches workspace v$WS_VERSION) =="
}
ensure_fresh_nub

WORK="$(mktemp -d "${TMPDIR:-/tmp}/nub-4way-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
for t in nub bun pnpm npm; do cp -R "$FIX" "$WORK/$t"; done

# A fixture ships every lockfile; each tool must install from its OWN. Strip the
# others so nub (which refuses an ambiguous multi-lockfile project) and the rest
# resolve the right one — same split as run.sh's setup_workdir. The nub leg keeps
# ONLY nub.lock so it exercises nub's native lockfile, not the pnpm-lock compat-read.
rm -f "$WORK/nub/pnpm-lock.yaml" "$WORK/nub/pnpm-workspace.yaml" "$WORK/nub/bun.lock" "$WORK/nub/bun.lockb" "$WORK/nub/package-lock.json"
rm -f "$WORK/pnpm/nub.lock" "$WORK/pnpm/bun.lock" "$WORK/pnpm/bun.lockb" "$WORK/pnpm/package-lock.json"
rm -f "$WORK/bun/nub.lock" "$WORK/bun/pnpm-lock.yaml" "$WORK/bun/pnpm-workspace.yaml" "$WORK/bun/package-lock.json"
rm -f "$WORK/npm/nub.lock" "$WORK/npm/bun.lock" "$WORK/npm/bun.lockb" "$WORK/npm/pnpm-lock.yaml" "$WORK/npm/pnpm-workspace.yaml"

echo "== warming each tool's store (untimed) =="
# nub is warmed twice: GVS on (populates the shared global virtual store) and GVS
# off (materialize path — reads the same warm CAS). Both nub bars install from the
# same $WORK/nub (nub.lock only), toggling GVS via the CI env var: env -u CI = on,
# CI=1 = off (the harness's existing GVS-pin mechanism).
( cd "$WORK/nub"  && env -u CI "$NUB" install --frozen-lockfile )
( cd "$WORK/nub"  && rm -rf node_modules && CI=1 "$NUB" install --frozen-lockfile )
( cd "$WORK/bun"  && bun install --frozen-lockfile )
( cd "$WORK/pnpm" && pnpm install --frozen-lockfile --silent )
( cd "$WORK/npm"  && npm ci )

# Confirm the two nub modes take the paths we label them. The robust cross-platform
# signal is the .store entry itself: with GVS ON, node_modules/.store/<pkg> is a
# SYMLINK into the shared global store (~/.cache/nub/pm/store/…); with GVS OFF it is
# a real per-project directory. (nlink is unreliable on macOS APFS, where the
# materialize path uses clonefile/COW and keeps nlink==1.) Reported untimed.
report_nub_layout() {
  local label="$1" ci_env="$2"
  rm -rf "$WORK/nub/node_modules"
  ( cd "$WORK/nub" && eval "$ci_env \"$NUB\" install --frozen-lockfile -s" ) >/dev/null 2>&1 || true
  local entry
  entry="$(find "$WORK/nub/node_modules/.store" -maxdepth 1 -mindepth 1 ! -name '.nub-state' 2>/dev/null | head -1)"
  if [ -z "$entry" ]; then
    echo "  [$label: no .store entries found]"
  elif [ -L "$entry" ]; then
    echo "  [$label: GVS ON — .store/$(basename "$entry") is a symlink into the shared global store]"
  else
    echo "  [$label: GVS OFF — .store/$(basename "$entry") is a per-project real dir (materialize)]"
  fi
}
echo "== nub linking-path check (untimed) =="
report_nub_layout "nub v0.3" "env -u CI"
report_nub_layout "nub v0.2" "CI=1"

echo "== warm install: $FIXTURE =="
# Five bars: nub v0.3 (GVS on, symlink-into-store relink) vs nub v0.2 (GVS off,
# per-project materialize — same binary, the path v0.2 shipped) vs bun/pnpm/npm.
#
# -N (no intermediate shell): hyperfine execs each command directly instead of via
# `sh -c`, so a per-run shell spawn (~1-3ms) doesn't compress the ratio between the
# sub-second tools. Every command is written shell-free: the nub bars use `env` for
# their var assignment (CI=1 is a shell assignment, not a binary — `env CI=1` is the
# execable form), and npm — which ignores --prefix and must cd — is wrapped in an
# explicit `sh -c` (only the slowest tool pays that shell, so the ratio is unaffected).
hyperfine -N --warmup "$WARMUP" --runs "$RUNS" \
  --command-name "nub v0.3"     --prepare "rm -rf '$WORK/nub/node_modules'"  "env -u CI '$NUB' --cwd '$WORK/nub' install --frozen-lockfile -s" \
  --command-name "nub v0.2"     --prepare "rm -rf '$WORK/nub/node_modules'"  "env CI=1 '$NUB' --cwd '$WORK/nub' install --frozen-lockfile -s" \
  --command-name "bun install"  --prepare "rm -rf '$WORK/bun/node_modules'"  "bun install --frozen-lockfile --cwd '$WORK/bun'" \
  --command-name "pnpm install" --prepare "rm -rf '$WORK/pnpm/node_modules'" "pnpm install --frozen-lockfile --dir '$WORK/pnpm' --silent" \
  --command-name "npm ci"       --prepare "rm -rf '$WORK/npm/node_modules'"  "sh -c \"cd '$WORK/npm' && npm ci --offline --ignore-scripts\""
