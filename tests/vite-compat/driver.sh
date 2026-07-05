#!/usr/bin/env bash
# driver.sh — validate nub's Vite symlink-GVS compat (#315) against one already-
# scaffolded, Vite-powered project. Given a project dir (containing package.json
# with a `vite` dep, directly or via a framework), it:
#
#   1. `nub install`   — confirms the symlink-GVS layout (vite realpath ⇒ store,
#                        or ejected project-local under compat) + records the
#                        exact Vite version the framework pinned.
#   2. checks Unit A   — node_modules/.modules.yaml written with the store path.
#   3. checks Unit B   — for Vite < 8.1, the dist backport patch is present.
#   4. dev serve       — starts the project's OWN dev server (bare, via the vite
#                        bin / framework CLI = WORKS-WITHOUT-NUB, no nub in the
#                        process) and fetches a store-resident module via `/@fs`;
#                        expects 200 (was 403) AND scans the dev log for the
#                        "outside of Vite serving allow list" 403 line.
#   5. build           — runs the project's build; expects success.
#
# Usage: driver.sh <project-dir> <dev-cmd> <build-cmd> <base-port>
#   <dev-cmd>/<build-cmd> run from inside <project-dir>; use "-" to skip build.
#   The dev server is expected to bind 127.0.0.1:<base-port> (pass --port/--host
#   through in <dev-cmd>).
#
# Env: NUB=<path to nub binary> (defaults to the fast dev build in the worktree).
#
# Fidelity note: this harness asserts at the HTTP layer — it fetches the REAL
# store-resident module the browser would request via `/@fs` and greps the dev
# server log for the actual 403 string. A chrome-devtools MCP browser pass
# (navigate + confirm hydration/interactivity + read console) is a stronger
# check when that MCP is available; the HTTP + log-scan pass is the CI-portable
# floor and is what this script encodes.
set -u

NUB="${NUB:-$HOME/.cache/nub/worktrees/vite-build-target/fast/nub}"
proj="$1"; dev_cmd="$2"; build_cmd="$3"; port="$4"
name="$(basename "$proj")"

fail() { echo "FAIL[$name]: $*" >&2; }

cd "$proj" || { fail "no dir"; exit 2; }

# ── 1. install ──────────────────────────────────────────────────────────────
"$NUB" install >/tmp/vc-$name-install.log 2>&1
inst=$?
[ $inst -eq 0 ] || { fail "nub install exit $inst"; tail -5 /tmp/vc-$name-install.log >&2; exit 2; }

vite_pkg="node_modules/vite/package.json"
[ -f "$vite_pkg" ] || { echo "SKIP[$name]: no vite in graph"; exit 0; }
vite_ver=$(node -p "require('./$vite_pkg').version" 2>/dev/null)

realpath_vite=$(node -e "console.log(require('fs').realpathSync('node_modules/vite'))")
case "$realpath_vite" in
  "$proj"/*) eject="ejected-local" ;;
  *)         eject="in-store" ;;
esac

# ── 2/3. Unit A + Unit B artifacts ──────────────────────────────────────────
modules_yaml="absent"; store=""
if [ -f node_modules/.modules.yaml ]; then
  modules_yaml="present"
  store=$(node -p "require('./node_modules/.modules.yaml').virtualStoreDir" 2>/dev/null)
fi
patched=$(grep -rl '__nubRfs' node_modules/vite/dist/node/ 2>/dev/null | wc -l | tr -d ' ')

# Version tier: < 8.1 exercises the BACKPORT; >= 8.1 the NATIVE sniff.
tier=$(node -e "const[a,b]=process.argv[1].split('.').map(Number);console.log(a>8||(a===8&&b>=1)?'native>=8.1':'backport<8.1')" "$vite_ver")

# ── 4. dev serve + /@fs of a real store-resident module (WORKS-WITHOUT-NUB) ──
served_code="n/a"; log_403="n/a"
target=""
if [ -n "$store" ]; then
  target=$(node -e "const fs=require('fs'),p=require('path');const s=process.argv[1];let hit='';for(const d of (fs.existsSync(s)?fs.readdirSync(s):[])){const nm=p.join(s,d,'node_modules');if(fs.existsSync(nm)){for(const m of fs.readdirSync(nm)){const f=p.join(nm,m,'package.json');if(fs.existsSync(f)){hit=fs.realpathSync(f);break}}}if(hit)break}console.log(hit)" "$store")
fi
if [ -n "$dev_cmd" ] && [ "$dev_cmd" != "-" ]; then
  ( cd "$proj" && eval "$dev_cmd" ) >/tmp/vc-$name-dev.log 2>&1 &
  devpid=$!
  up=""
  for i in $(seq 1 40); do
    sleep 0.5
    curl -s -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null && { up=1; break; }
  done
  if [ -n "$up" ] && [ -n "$target" ]; then
    served_code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/@fs$target")
  fi
  # scan for the literal Vite 403 line
  if grep -q "outside of Vite serving allow list\|is not allowed" /tmp/vc-$name-dev.log 2>/dev/null; then
    log_403="403-in-log"
  else
    log_403="no-403"
  fi
  kill $devpid 2>/dev/null; wait $devpid 2>/dev/null
  pkill -f "vite.*--port $port" 2>/dev/null
fi

# ── 5. build ────────────────────────────────────────────────────────────────
build_res="skip"
if [ -n "$build_cmd" ] && [ "$build_cmd" != "-" ]; then
  ( cd "$proj" && eval "$build_cmd" ) >/tmp/vc-$name-build.log 2>&1
  [ $? -eq 0 ] && build_res="ok" || build_res="FAIL"
fi

echo "ROW|$name|vite=$vite_ver|$tier|$eject|modules.yaml=$modules_yaml|patched=$patched|/@fs=$served_code|log=$log_403|build=$build_res"
