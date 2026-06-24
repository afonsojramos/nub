#!/usr/bin/env bash
# tart-vm.sh — spin up a headless Linux VM on Apple Silicon, run a command/script
# inside it, capture the output on the host, and tear it down. The fast,
# free, fully-CLI-scriptable VM loop for cross-platform probing on macOS/arm64.
#
# Backend: Tart (Apple Virtualization.framework). Linux + macOS guests only —
# Windows-on-ARM is NOT supported by Apple's framework (see ../windows + the
# QEMU runbook referenced in README.md). For a Linux probe this is the loop.
#
# Proven end-to-end 2026-06-23 on M1 Max / macOS 26.5 with tart 2.32.1 against
# ghcr.io/cirruslabs/ubuntu:latest (Ubuntu 24.04 arm64). See README.md.
#
# Usage:
#   tart-vm.sh up    [name] [image]   # clone + boot headless, wait for IP
#   tart-vm.sh exec  [name] -- CMD... # run CMD in the guest, output to host stdout
#   tart-vm.sh run   [name] FILE      # copy a local script into the guest and run it
#   tart-vm.sh ip    [name]           # print the guest IP
#   tart-vm.sh down  [name]           # stop the VM (disk persists)
#   tart-vm.sh rm    [name]           # stop + delete the VM (frees disk)
#   tart-vm.sh demo                   # full up→exec→run→down on a throwaway VM
#
# Defaults: name=nub-linux-proof, image=ghcr.io/cirruslabs/ubuntu:latest.
# cirruslabs Linux images use credentials admin/admin (used here for scp).
set -uo pipefail

NAME_DEFAULT="nub-linux-proof"
IMAGE_DEFAULT="ghcr.io/cirruslabs/ubuntu:latest"
GUEST_USER="admin"
GUEST_PASS="admin"

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing dependency: $1"; }

wait_for_ip() {
  local name="$1" ip
  for _ in $(seq 1 60); do
    ip=$(tart ip "$name" 2>/dev/null)
    if [ -n "$ip" ]; then echo "$ip"; return 0; fi
    sleep 5
  done
  return 1
}

cmd_up() {
  local name="${1:-$NAME_DEFAULT}" image="${2:-$IMAGE_DEFAULT}"
  need tart
  if ! tart list 2>/dev/null | awk '{print $2}' | grep -qx "$name"; then
    echo ">> cloning $image -> $name" >&2
    tart clone "$image" "$name" || die "clone failed"
  fi
  echo ">> booting $name headless" >&2
  tart run --no-graphics "$name" >/tmp/tart-$name.log 2>&1 &
  local ip
  ip=$(wait_for_ip "$name") || die "VM never got an IP (see /tmp/tart-$name.log)"
  echo ">> $name up at $ip" >&2
  echo "$ip"
}

cmd_exec() {
  # Optional leading [name]; if the first arg is the `--` separator (or absent),
  # the name was omitted -> use the default.
  local name="$NAME_DEFAULT"
  if [ "${1:-}" != "--" ] && [ "$#" -gt 0 ]; then name="$1"; shift; fi
  [ "${1:-}" = "--" ] && shift
  [ "$#" -gt 0 ] || die "exec needs a command after --"
  need tart
  tart exec "$name" "$@"
}

cmd_run() {
  # Optional leading [name]; if the first arg is an existing file, the name was
  # omitted -> use the default and treat that arg as the script.
  local name="$NAME_DEFAULT" file
  if [ -f "${1:-}" ]; then file="$1"; else name="${1:-$NAME_DEFAULT}"; file="${2:-}"; fi
  [ -n "$file" ] || die "run needs a local script path"
  [ -f "$file" ] || die "no such file: $file"
  need tart
  local ip; ip=$(tart ip "$name" 2>/dev/null) || die "$name has no IP — is it up?"
  local remote="/tmp/$(basename "$file")"
  if command -v sshpass >/dev/null 2>&1; then
    sshpass -p "$GUEST_PASS" scp -q -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null "$file" "$GUEST_USER@$ip:$remote" \
      || die "scp failed"
    tart exec "$name" bash "$remote"
  else
    # No sshpass: pipe the script body through tart exec stdin.
    tart exec "$name" bash -c "$(cat "$file")"
  fi
}

cmd_ip()   { tart ip "${1:-$NAME_DEFAULT}"; }
cmd_down() { tart stop "${1:-$NAME_DEFAULT}"; }
cmd_rm()   { local n="${1:-$NAME_DEFAULT}"; tart stop "$n" 2>/dev/null; tart delete "$n"; }

cmd_demo() {
  local name="nub-vm-demo-$$"
  echo "== VM-control demo (throwaway VM: $name) =="
  cmd_up "$name" >/dev/null
  echo "-- exec: guest identity --"
  cmd_exec "$name" -- bash -lc \
    'echo "ARCH=$(uname -m)"; echo "DISTRO=$(. /etc/os-release; echo $PRETTY_NAME)"; echo "WHOAMI=$(whoami)"'
  echo "-- run: a local probe script --"
  local probe; probe=$(mktemp /tmp/vm-probe.XXXXXX.sh)
  cat > "$probe" <<'EOF'
echo "probe on: $(uname -m) / $(hostname)"
echo "fs write /tmp: $(touch /tmp/marker && echo ok)"
echo "egress https: $(curl -s -o /dev/null -w '%{http_code}' --max-time 5 https://example.com || echo blocked)"
EOF
  cmd_run "$name" "$probe"
  rm -f "$probe"
  echo "-- teardown --"
  cmd_rm "$name"
  echo "== demo complete =="
}

sub="${1:-}"; shift || true
case "$sub" in
  up)   cmd_up   "$@" ;;
  exec) cmd_exec "$@" ;;
  run)  cmd_run  "$@" ;;
  ip)   cmd_ip   "$@" ;;
  down) cmd_down "$@" ;;
  rm)   cmd_rm   "$@" ;;
  demo) cmd_demo "$@" ;;
  *) sed -n '2,30p' "$0"; exit 1 ;;
esac
