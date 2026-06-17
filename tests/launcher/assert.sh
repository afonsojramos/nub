#!/bin/sh
# Runs INSIDE the non-owner container as the non-root `app` user (see
# Dockerfile.non-owner). The native binary is root-owned 0o644 and postinstall never
# ran, so the ONLY thing that can make `nub` work is ensureExecutable's non-owner
# branch: stage a user-owned 0o755 copy under ~/.cache/nub/bin/<tag>/ and exec it.
#
# Note: PATH puts /opt/nub/bin first; `node` is the real /usr/local/bin/node (we are
# NOT using the spawn-logging fakenode here — this leg tests staged-copy recovery,
# not the zero-node fast path, which run-launcher-matrix.sh covers on the host).
set -u
PASS=0; FAIL=0
ok(){ echo "  ok: $1"; PASS=$((PASS+1)); }
no(){ echo "  FAIL: $1"; FAIL=$((FAIL+1)); }

NATIVE=/opt/nub/node_modules/@nubjs/nub-host/bin/nub

# Precondition sanity: we are non-root, the native is NOT owned by us and NOT +x.
[ "$(id -u)" != "0" ] && ok "running as non-root ($(id -un))" || no "expected non-root"
if [ -x "$NATIVE" ]; then no "precondition: native is already +x (test is moot)"; else ok "precondition: native is 0o644 (no +x)"; fi
# Confirm we genuinely cannot chmod it (not the owner) — the whole reason the staged
# copy exists. `chmod` as non-owner of a root file fails.
if chmod +x "$NATIVE" 2>/dev/null; then no "precondition: non-root could chmod the native (not actually a non-owner case)"; else ok "precondition: cannot chmod native (not owner)"; fi

# THE TEST: first `nub` as the non-root user. Must succeed via the staged copy.
out=$(nub --version 2>&1)
case "$out" in
  *9.9.9-ci*) ok "first nub works for non-root via staged copy";;
  *) no "first nub FAILED: $out";;
esac

# A user-owned, executable copy must now exist under ~/.cache/nub/bin/.
CACHE="$HOME/.cache/nub/bin"
staged=$(find "$CACHE" -type f -name nub 2>/dev/null | head -1)
if [ -n "$staged" ]; then
  ok "staged copy exists: ${staged#$HOME/}"
  [ -x "$staged" ] && ok "staged copy is executable" || no "staged copy not +x"
  [ -O "$staged" ] && ok "staged copy is owned by us" || no "staged copy not owned by us"
  # The staleness tag is the directory name (<size>-<mtime>), so the executable keeps
  # its bare verb name — required for argv0 verb dispatch.
  case "$(basename "$staged")" in nub) ok "staged file keeps bare verb name (argv0 dispatch)";; *) no "staged file misnamed: $(basename "$staged")";; esac
else
  no "no staged copy under $CACHE"
fi

# Second call must also work (re-uses the staged copy; no error).
out2=$(nub --version 2>&1)
case "$out2" in *9.9.9-ci*) ok "second nub still works (staged copy reused)";; *) no "second nub FAILED: $out2";; esac

echo
echo "RESULT: $PASS ok, $FAIL failed"
[ "$FAIL" -eq 0 ]
