#!/usr/bin/env bash
# run.sh — one framework's out-of-the-box acceptance run for the fresh-build
# matrix. Given an already-scaffolded project, it exercises the THREE distinct
# code paths (dev server, production build, serve-the-build) and asserts each,
# plus verifies the symlink-GVS / eject linking layout is actually in use.
#
# Usage: run.sh <name> <proj-dir> <dev-cmd> <build-cmd> <preview-cmd> [dep-to-probe]
#   Commands run from inside <proj-dir>; "-" skips a step. Server commands must
#   bind 127.0.0.1 and print their URL to stdout/stderr (the port is parsed from
#   the log; pass --port/--host through where the framework needs it).
#   <dep-to-probe> is a node_modules dep whose realpath decides the layout verdict
#   (default: pick the first top-level dep). Set NUB=<path> to the nub binary.
#
# Emits one ROW| line per step plus a final VERDICT| line. Behavioral assertions
# (HTTP status, build exit code) are load-INDEPENDENT — no wall-clock gating.
set -u

NUB="${NUB:?set NUB to the nub binary path}"
name="$1"; proj="$2"; dev_cmd="$3"; build_cmd="$4"; preview_cmd="$5"; probe="${6:-}"

log() { echo "ROW|$name|$*"; }

kill_tree() { local p="$1" c; for c in $(pgrep -P "$p" 2>/dev/null); do kill_tree "$c"; done; kill -TERM "$p" 2>/dev/null; }
kill_group() { local g="$1"; kill -TERM -"$g" 2>/dev/null; sleep 0.4; kill -KILL -"$g" 2>/dev/null; kill_tree "$g"; kill -KILL "$g" 2>/dev/null; }

cd "$proj" || { log "step=setup result=FAIL detail=no-dir"; echo "VERDICT|$name|FAIL"; exit 2; }

# ── install ──────────────────────────────────────────────────────────────────
"$NUB" install >/tmp/fm-$name-install.log 2>&1; inst=$?
if [ $inst -ne 0 ]; then
  log "step=install result=FAIL exit=$inst"; tail -8 /tmp/fm-$name-install.log | sed "s/^/  install> /"
  echo "VERDICT|$name|FAIL"; exit 2
fi
log "step=install result=ok"

# ── linking layout: realpath a real dep, does it escape the project root? ─────
# escape → symlink into the global virtual store (GVS on); stays inside proj →
# ejected / disableGVS project-local. Reports the resolved realpath either way.
read -r layout rp probe_used < <(NUBPROBE="$probe" node -e '
const fs=require("fs"),p=require("path"),cwd=process.cwd();
const nm=p.join(cwd,"node_modules");
function realOf(name){try{const l=p.join(nm,name);const st=fs.lstatSync(l);const r=fs.realpathSync(l);return r;}catch{return null}}
let probe=process.env.NUBPROBE||"";
let cand=[];
if(probe) cand=[probe];
else {
  // pick a few ordinary deps, skip .bin/.nub/.modules and scoped-dir noise
  for(const e of fs.readdirSync(nm)){ if(e[0]==="."||e==="node_modules") continue; if(e[0]==="@"){ for(const s of fs.readdirSync(p.join(nm,e))) cand.push(e+"/"+s);} else cand.push(e);}
}
let chosen=null,real=null;
for(const c of cand){ const r=realOf(c); if(r){chosen=c;real=r;break;} }
if(!real){console.log("unknown - -");process.exit(0)}
const escapes=!real.startsWith(cwd+p.sep);
console.log((escapes?"gvs-store":"project-local")+" "+real+" "+chosen);
' 2>/dev/null)
log "step=linking layout=${layout:-err} probe=${probe_used:-?} realpath=${rp:-?}"

# ── dev server ────────────────────────────────────────────────────────────────
dev_result="skip"; dev_code="n/a"; dev_err="n/a"
if [ "$dev_cmd" != "-" ]; then
  devlog=/tmp/fm-$name-dev.log; : >"$devlog"
  set -m; ( cd "$proj" && exec $dev_cmd ) >"$devlog" 2>&1 & devpid=$!; set +m
  bound=""; up=""
  for i in $(seq 1 120); do
    sleep 0.5
    pp=$(grep -oE 'https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):[0-9]+' "$devlog" 2>/dev/null | grep -oE '[0-9]+$' | head -1)
    [ -n "$pp" ] && bound="$pp"
    if [ -n "$bound" ]; then curl -s -o /dev/null "http://localhost:$bound/" 2>/dev/null && { up=1; break; }; fi
    kill -0 "$devpid" 2>/dev/null || break   # dev process died
  done
  if [ -n "$up" ]; then
    dev_code=$(curl -s -o /tmp/fm-$name-dev-body.html -w '%{http_code}' "http://localhost:$bound/" 2>/dev/null)
    # error scan: server log + served HTML for the common SSR/runtime error markers
    if grep -qiE 'error:|ReferenceError|TypeError|Cannot find|MODULE_NOT_FOUND|ERR_|Internal Server Error|failed to (load|resolve)|is not allowed|outside of .* allow list|Unhandled' "$devlog" 2>/dev/null; then dev_err="log-errors"; else dev_err="clean"; fi
    grep -qiE 'Internal Server Error|Application error|Hydration failed|500 - ' /tmp/fm-$name-dev-body.html 2>/dev/null && dev_err="html-error"
    [ "$dev_code" = "200" ] && [ "$dev_err" = "clean" ] && dev_result="ok" || dev_result="FAIL"
  else
    dev_result="FAIL"; dev_code="no-listen"
  fi
  kill_group "$devpid"; wait "$devpid" 2>/dev/null
  log "step=dev result=$dev_result http=$dev_code errors=$dev_err"
  [ "$dev_result" = "FAIL" ] && { echo "  dev-log tail:"; tail -15 "$devlog" | sed 's/^/  dev> /'; }
fi

# ── production build ──────────────────────────────────────────────────────────
build_result="skip"
if [ "$build_cmd" != "-" ]; then
  ( cd "$proj" && eval "$build_cmd" ) >/tmp/fm-$name-build.log 2>&1; bx=$?
  [ $bx -eq 0 ] && build_result="ok" || build_result="FAIL"
  log "step=build result=$build_result exit=$bx"
  [ "$build_result" = "FAIL" ] && { echo "  build-log tail:"; tail -20 /tmp/fm-$name-build.log | sed 's/^/  build> /'; }
fi

# ── serve the built output (preview / start) ─────────────────────────────────
prev_result="skip"; prev_code="n/a"
if [ "$preview_cmd" != "-" ] && [ "$build_result" != "FAIL" ]; then
  prevlog=/tmp/fm-$name-preview.log; : >"$prevlog"
  set -m; ( cd "$proj" && exec $preview_cmd ) >"$prevlog" 2>&1 & prevpid=$!; set +m
  pbound=""; pup=""
  for i in $(seq 1 120); do
    sleep 0.5
    pp=$(grep -oE 'https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):[0-9]+' "$prevlog" 2>/dev/null | grep -oE '[0-9]+$' | head -1)
    [ -n "$pp" ] && pbound="$pp"
    if [ -n "$pbound" ]; then curl -s -o /dev/null "http://localhost:$pbound/" 2>/dev/null && { pup=1; break; }; fi
    kill -0 "$prevpid" 2>/dev/null || break
  done
  if [ -n "$pup" ]; then
    prev_code=$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$pbound/" 2>/dev/null)
    [ "$prev_code" = "200" ] && prev_result="ok" || prev_result="FAIL"
  else
    prev_result="FAIL"; prev_code="no-listen"
  fi
  kill_group "$prevpid"; wait "$prevpid" 2>/dev/null
  log "step=preview result=$prev_result http=$prev_code"
  [ "$prev_result" = "FAIL" ] && { echo "  preview-log tail:"; tail -15 "$prevlog" | sed 's/^/  prev> /'; }
fi

# ── verdict ──────────────────────────────────────────────────────────────────
verdict="PASS"
for r in "$dev_result" "$build_result" "$prev_result"; do [ "$r" = "FAIL" ] && verdict="FAIL"; done
echo "VERDICT|$name|$verdict|dev=$dev_result build=$build_result preview=$prev_result layout=${layout:-err}"
