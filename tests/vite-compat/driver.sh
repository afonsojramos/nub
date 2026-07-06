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

# Kill a dev server AND its whole descendant tree. A dev server (`astro dev`,
# `nuxt dev`, `vite`) forks workers (the optimizer, Nitro, an esbuild/SWC service)
# that a bare `kill $pid` — and even a `pgrep -P` walk — miss once a grandchild
# reparents to init before the walk snapshots it, so a worker LEAKS onto the port;
# the next run then binds a DIFFERENT port while the harness curls the stale
# (unpatched) server → false 403s. The robust fix is a process GROUP: the dev
# server is launched as its own group leader (via `set -m`, so PGID == the job
# pid), and `kill -SIGNAL -PGID` signals the ENTIRE group atomically — no
# reparenting race, no missed grandchild. `pgrep -P` recursion remains only as a
# best-effort fallback for a process that somehow escaped the group.
kill_tree() {
  local p="$1" c
  for c in $(pgrep -P "$p" 2>/dev/null); do kill_tree "$c"; done
  kill -TERM "$p" 2>/dev/null
}

# Tear down the dev server's process group, then KILL-sweep, then fall back to the
# pgrep walk. `-$pgid` (negative) targets the group; a leading `kill 0`-style
# guard is unnecessary because $pgid is a real job-leader pid captured under -m.
kill_group() {
  local pgid="$1"
  kill -TERM -"$pgid" 2>/dev/null
  sleep 0.3
  kill -KILL -"$pgid" 2>/dev/null
  kill_tree "$pgid"
  kill -KILL "$pgid" 2>/dev/null
}

cd "$proj" || { fail "no dir"; exit 2; }

# ── 1. install ──────────────────────────────────────────────────────────────
"$NUB" install >/tmp/vc-$name-install.log 2>&1
inst=$?
[ $inst -eq 0 ] || { fail "nub install exit $inst"; tail -5 /tmp/vc-$name-install.log >&2; exit 2; }

# Vite reaches the graph as a direct dep (top-level node_modules/vite) OR
# transitively as a framework's embedded engine (only .nub/vite@* entries, no
# top-level symlink). Detect + resolve the LOADED vite either way: prefer what
# the framework/CLI actually imports (node resolution), else the first .nub entry.
read -r vite_ver realpath_vite < <(node -e '
const fs=require("fs"),p=require("path");
function ver(d){try{return require(p.join(d,"package.json")).version}catch{return null}}
let dir=null;
try{ // what would `vite dev` / the framework load?
  dir=p.dirname(fs.realpathSync(require.resolve("vite/package.json",{paths:[process.cwd()]})));
}catch{}
if(!dir){ // transitive-only: scan .nub/vite@*
  const nb=p.join("node_modules",".nub");
  for(const e of (fs.existsSync(nb)?fs.readdirSync(nb):[])){
    if(e.startsWith("vite@")){const c=p.join(nb,e,"node_modules","vite");if(fs.existsSync(c)){dir=fs.realpathSync(c);break}}
  }
}
if(!dir){console.log("- -");process.exit(0)}
console.log((ver(dir)||"?")+" "+dir);
')
[ "$vite_ver" = "-" ] && { echo "SKIP[$name]: no vite in graph"; exit 0; }
case "$realpath_vite" in
  "$proj"/*) eject="ejected-local" ;;
  *)         eject="in-store" ;;
esac

# ── 2/3. Unit A + Unit B artifacts ──────────────────────────────────────────
modules_yaml="absent"; store=""
if [ -f node_modules/.modules.yaml ]; then
  modules_yaml="present"
  # NOT require() — Node has no loader for a .yaml extension (falls to the .js
  # handler and throws on the JSON body). Parse the bytes directly.
  store=$(node -e "console.log(JSON.parse(require('fs').readFileSync('node_modules/.modules.yaml','utf8')).virtualStoreDir)" 2>/dev/null)
fi
# Patch count on the ACTUALLY-LOADED vite (realpath) — the ejected project-local
# copy for a direct dep; the store copy (never patched) for library-embedded.
patched=$(grep -rl '__nubRfs' "$realpath_vite/dist/node/" 2>/dev/null | wc -l | tr -d ' ')

# Version tier: < 8.1 exercises the BACKPORT; >= 8.1 the NATIVE sniff.
tier=$(node -e "const[a,b]=process.argv[1].split('.').map(Number);console.log(a>8||(a===8&&b>=1)?'native>=8.1':'backport<8.1')" "$vite_ver")

# ── 4. dev serve + /@fs of a real store-resident module (WORKS-WITHOUT-NUB) ──
served_code="n/a"; log_403="n/a"
target=""
if [ -n "$store" ]; then
  target=$(node -e "const fs=require('fs'),p=require('path');const s=process.argv[1];let hit='';for(const d of (fs.existsSync(s)?fs.readdirSync(s):[])){const nm=p.join(s,d,'node_modules');if(fs.existsSync(nm)){for(const m of fs.readdirSync(nm)){const f=p.join(nm,m,'package.json');if(fs.existsSync(f)){hit=fs.realpathSync(f);break}}}if(hit)break}console.log(hit)" "$store")
fi
if [ -n "$dev_cmd" ] && [ "$dev_cmd" != "-" ]; then
  devlog=/tmp/vc-$name-dev.log
  : >"$devlog"
  # `set -m` (job control) makes this background job its OWN process-group leader,
  # so `devpid` is both the group leader PID and the PGID — `kill_group` can then
  # tear down the whole tree via `-$devpid`. `exec` keeps devpid pinned to the real
  # dev process (astro/nuxt/vite), not a wrapper subshell.
  set -m
  ( cd "$proj" && exec $dev_cmd ) >"$devlog" 2>&1 &
  devpid=$!
  set +m
  # Parse the ACTUAL bound port from the dev log — a framework re-picks a free
  # port if the requested one is taken (e.g. after a prior leak), so curling the
  # requested port would hit the wrong/stale server. Fall back to the requested.
  bound="$port"; up=""
  for i in $(seq 1 60); do
    sleep 0.5
    pp=$(grep -oE 'https?://(localhost|127\.0\.0\.1|\[::1\]):[0-9]+' "$devlog" 2>/dev/null \
           | grep -oE '[0-9]+$' | head -1)
    [ -n "$pp" ] && bound="$pp"
    curl -s -o /dev/null "http://127.0.0.1:$bound/" 2>/dev/null && { up=1; break; }
  done
  if [ -n "$up" ] && [ -n "$target" ]; then
    served_code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$bound/@fs$target")
  fi
  # scan for the literal Vite 403 line
  if grep -q "outside of Vite serving allow list\|is not allowed" "$devlog" 2>/dev/null; then
    log_403="403-in-log"
  else
    log_403="no-403"
  fi
  kill_group "$devpid"; wait "$devpid" 2>/dev/null
fi

# ── 5. build ────────────────────────────────────────────────────────────────
build_res="skip"
if [ -n "$build_cmd" ] && [ "$build_cmd" != "-" ]; then
  ( cd "$proj" && eval "$build_cmd" ) >/tmp/vc-$name-build.log 2>&1
  [ $? -eq 0 ] && build_res="ok" || build_res="FAIL"
fi

echo "ROW|$name|vite=$vite_ver|$tier|$eject|modules.yaml=$modules_yaml|patched=$patched|/@fs=$served_code|log=$log_403|build=$build_res"
