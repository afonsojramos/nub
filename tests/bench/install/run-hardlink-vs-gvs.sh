#!/usr/bin/env bash
# Honest apples-to-apples warm-install bench on LINUX: prove nub's link
# machinery matches bun IN BUN'S OWN REGIME (flat hoisted layout + per-file
# hardlink = same syscall), then show the global virtual store (GVS) is a
# genuine O(packages) algorithmic unlock on top.
#
# Why Linux: `auto` package-import-method resolves to a hardlink on Linux
# (btrfs/xfs/ext4) for BOTH bun and nub, so the two share the SAME primitive
# and the comparison is clean. On macOS `auto` resolves to APFS clonefile for
# both, which muddies a hardlink-vs-GVS story — run this on Linux only.
#
# The three headline bars:
#   1. bun            — flat hoisted node_modules, per-file hardlink from
#                       ~/.bun/install/cache. Baseline.
#   2. nub-hoisted-hl — `nub install --node-linker hoisted`: flat hoisted
#                       node_modules, per-file hardlink from the CAS. SAME
#                       layout + SAME syscall as bun, so this neutralizes any
#                       "different algorithm" objection — it is bun's own regime.
#                       (`--node-linker hoisted` never engages GVS: see aube
#                       gvs.rs `hoisted_layout_never_uses_shared_store`; and off
#                       macOS there is no whole-dir clonedir fast path — aube
#                       link.rs `try_clonedir_fill` is `#[cfg(not(macos))] ->
#                       Ok(false)` — so every file materializes via a per-file
#                       `std::fs::hard_link`.)
#   3. nub-gvs        — nub at DEFAULTS (isolated layout + GVS on): node_modules
#                       is a symlink farm into one shared global virtual store,
#                       so a warm install is O(packages) symlinks instead of
#                       O(files) hardlinks. THE unlock.
# + pnpm, npm for context.
#
# The story: bar 1 vs bar 2 = we earned it honestly (match bun's hardlink
# first, in bun's own layout); bar 2 vs bar 3 = GVS is a real algorithmic leap.
#
# GROUNDING: before the timed run, each nub bar is executed once under
# NUB_DIAG_FILE and the realized link-strategy tally (link_hardlink /
# link_clonedir / link_reflink / link_copy) + the `phase:link` file count are
# printed. No strategy claim rests on source-reading — the tally proves bar 2 is
# genuine per-file hardlink and bar 3 is the symlink relink.
#
# Warm install = warm global store + frozen lockfile, node_modules wiped between
# runs (in hyperfine --prepare, excluded from timing), no network. Each tool
# gets its own copy of the fixture; each store is pre-warmed once (untimed)
# before the timed run.
#
# Usage:
#   cargo build --release -p nub-cli
#   bash tests/bench/install/run-hardlink-vs-gvs.sh
#   NUB=/path/to/nub bash tests/bench/install/run-hardlink-vs-gvs.sh --fixture large --runs 20 --warmup 5
#
# Requires: hyperfine, pnpm, bun, npm on PATH; a release nub (NUB= or target/release/nub).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
NUB="${NUB:-$REPO_ROOT/target/release/nub}"
case "$NUB" in /*) ;; *) NUB="$(cd "$(dirname "$NUB")" && pwd)/$(basename "$NUB")" ;; esac

FIXTURE="large"   # fat, GVS-eligible (1168 pkgs, no next/nuxt/parcel) — the
                  # file-heavy tree that widens the O(files) vs O(packages) gap.
RUNS=20
WARMUP=5
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
[ -x "$NUB" ] || { echo "nub not built at $NUB (run: cargo build --release -p nub-cli)" >&2; exit 1; }
command -v hyperfine >/dev/null || { echo "hyperfine not found (brew install hyperfine)" >&2; exit 1; }

case "$(uname -s)" in
  Linux) ;;
  *) echo "WARNING: this bench is meaningful only on Linux (auto->hardlink for both tools);" >&2
     echo "         on $(uname -s) auto resolves to clonefile and the hardlink-vs-GVS story is muddied." >&2 ;;
esac

# Staleness guard: the freshly-built binary's version must match the workspace
# Cargo.toml version, or the numbers describe a stale binary. Fatal — a bench of
# the wrong binary is worse than no bench.
WS_VER="$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
NUB_VER="$("$NUB" --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
if [ -n "$WS_VER" ] && [ -n "$NUB_VER" ] && [ "$WS_VER" != "$NUB_VER" ]; then
  echo "STALE BINARY: nub reports $NUB_VER but workspace Cargo.toml is $WS_VER — rebuild." >&2
  exit 1
fi
echo "binary version $NUB_VER matches workspace $WS_VER (staleness guard OK)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/nub-hlgvs-XXXXXX")"
DIAG_DIR="$WORK/diag"; mkdir -p "$DIAG_DIR"
trap 'rm -rf "$WORK"' EXIT
# nub-hoisted and nub-gvs get SEPARATE copies so a layout switch (hoisted vs
# isolated) between bars never triggers aube's mode-change node_modules wipe
# (issue #71) inside a timed run.
for t in nub-hoisted nub-gvs bun pnpm npm; do cp -R "$FIX" "$WORK/$t"; done

# A fixture ships every lockfile; each tool must install from its OWN, and nub
# REFUSES a project that carries two lockfiles it could own (nub.lock +
# pnpm-lock.yaml => ERR_NUB_LOCKFILE_AMBIGUOUS). So each nub dir keeps ONLY
# nub.lock (nub's native lockfile — the honest nub leg), and every other dir
# has nub.lock removed. Mirrors run-4way.sh's split.
for d in nub-hoisted nub-gvs; do
  rm -f "$WORK/$d/pnpm-lock.yaml" "$WORK/$d/pnpm-workspace.yaml" "$WORK/$d/bun.lock" "$WORK/$d/bun.lockb" "$WORK/$d/package-lock.json"
done
rm -f "$WORK/pnpm/nub.lock" "$WORK/pnpm/bun.lock" "$WORK/pnpm/bun.lockb" "$WORK/pnpm/package-lock.json"
rm -f "$WORK/bun/nub.lock" "$WORK/bun/pnpm-lock.yaml" "$WORK/bun/pnpm-workspace.yaml" "$WORK/bun/package-lock.json"
rm -f "$WORK/npm/nub.lock" "$WORK/npm/bun.lock" "$WORK/npm/bun.lockb" "$WORK/npm/pnpm-lock.yaml" "$WORK/npm/pnpm-workspace.yaml"

echo "== warming each tool's store (untimed) =="
( cd "$WORK/nub-hoisted" && "$NUB" install --frozen-lockfile --node-linker hoisted -s )
( cd "$WORK/nub-gvs"     && env -u CI "$NUB" install --frozen-lockfile -s )
( cd "$WORK/bun"         && bun install --frozen-lockfile )
( cd "$WORK/pnpm"        && pnpm install --frozen-lockfile --silent )
( cd "$WORK/npm"         && npm ci )

# ── Strategy grounding (untimed) ──────────────────────────────────────────────
# Run each nub bar once under NUB_DIAG_FILE + RUST_LOG=debug, then tally the
# realized per-file link strategy from the JSONL and pull the phase:link file
# count. Proves bar 2 = per-file hardlink (same syscall as bun) and bar 3 = the
# symlink relink, on THIS machine, not from source.
ground_nub_bar() {
  local dir="$1" label="$2" diag="$3"; shift 3
  rm -rf "$dir/node_modules"
  local phaseline
  phaseline="$(NUB_DIAG_FILE="$diag" RUST_LOG=debug "$@" 2>&1 | grep -oE 'phase:link [0-9.]+m?s \([0-9]+ files\)' | tail -1 || true)"
  local hl cl rl cp sm
  hl=$(grep -c '"name":"link_hardlink"' "$diag" 2>/dev/null || true)
  cl=$(grep -c '"name":"link_clonedir"' "$diag" 2>/dev/null || true)
  rl=$(grep -c '"name":"link_reflink"' "$diag" 2>/dev/null || true)
  cp=$(grep -c '"name":"link_copy"' "$diag" 2>/dev/null || true)
  sm=$(grep -c '"name":"link_macos_small_copy"' "$diag" 2>/dev/null || true)
  echo "  [$label]"
  echo "    ${phaseline:-phase:link (not captured)}"
  echo "    per-file link tally: hardlink=$hl  clonedir=$cl  reflink=$rl  copy=$cp  small_copy=$sm"
}

echo ""
echo "== strategy grounding (untimed; diag tally) =="
# NB: no `-s` here — the progress-silent flag also suppresses the RUST_LOG
# tracing, and the phase:link file-count line rides on it. The NUB_DIAG_FILE
# JSONL tally is written regardless of -s. (The timed hyperfine bars below keep
# -s so the progress UI is out of the measurement.)
ground_nub_bar "$WORK/nub-hoisted" "nub-hoisted-hl (bar 2): expect hardlink==files, clonedir==0" \
  "$DIAG_DIR/hoisted.jsonl" "$NUB" --cwd "$WORK/nub-hoisted" install --frozen-lockfile --node-linker hoisted
ground_nub_bar "$WORK/nub-gvs" "nub-gvs (bar 3): expect near-zero per-file links (warm store -> symlink relink)" \
  "$DIAG_DIR/gvs.jsonl" env -u CI "$NUB" --cwd "$WORK/nub-gvs" install --frozen-lockfile
# Report node_modules shape (flat real dirs for hoisted; symlink farm for GVS).
echo "  [layout]"
if [ -d "$WORK/nub-hoisted/node_modules/react" ] && [ ! -L "$WORK/nub-hoisted/node_modules/react" ]; then
  echo "    nub-hoisted: node_modules/react is a REAL dir (flat hoisted, like bun) ✓"
fi
if [ -L "$WORK/nub-gvs/node_modules/react" ] || [ -d "$WORK/nub-gvs/node_modules/.store" ] || [ -d "$WORK/nub-gvs/node_modules/.aube" ]; then
  echo "    nub-gvs: node_modules is a symlink farm into the shared store ✓"
fi
# Re-warm the two nub dirs consumed by grounding wipes.
( cd "$WORK/nub-hoisted" && "$NUB" install --frozen-lockfile --node-linker hoisted -s )
( cd "$WORK/nub-gvs"     && env -u CI "$NUB" install --frozen-lockfile -s )

echo ""
echo "== warm install: $FIXTURE ($RUNS runs / $WARMUP warmup) =="
# -N (no intermediate shell) so a per-run shell spawn doesn't add a fixed cost.
# Order tells the story: bun (baseline) -> nub-hoisted-hl (same regime) -> nub-gvs (the unlock).
hyperfine -N --warmup "$WARMUP" --runs "$RUNS" \
  --command-name "bun"            --prepare "rm -rf '$WORK/bun/node_modules'"         "bun install --frozen-lockfile --cwd '$WORK/bun'" \
  --command-name "nub-hoisted-hl" --prepare "rm -rf '$WORK/nub-hoisted/node_modules'" "'$NUB' --cwd '$WORK/nub-hoisted' install --frozen-lockfile --node-linker hoisted -s" \
  --command-name "nub-gvs"        --prepare "rm -rf '$WORK/nub-gvs/node_modules'"     "env -u CI '$NUB' --cwd '$WORK/nub-gvs' install --frozen-lockfile -s" \
  --command-name "pnpm"           --prepare "rm -rf '$WORK/pnpm/node_modules'"        "pnpm install --frozen-lockfile --dir '$WORK/pnpm' --silent" \
  --command-name "npm"            --prepare "rm -rf '$WORK/npm/node_modules'"         "sh -c \"cd '$WORK/npm' && npm ci --offline --ignore-scripts\""
