# Local VMs on Apple Silicon — spin up, exec, tear down

A CLI-scriptable way to boot a throwaway VM on a macOS/arm64 host, run a command or probe script inside it, capture the output back on the host, and tear it down. The point is to turn a "does X behave this way on another OS?" question into a local experiment instead of a CI round-trip — cross-distro Linux sandbox probing, floor-Node testing, clean-machine install runs, and (the original driver) the Windows script-sandbox probes.

Two backends, because no single tool covers both guest families on Apple Silicon:

| Guest | Tool | Speed | Status |
| --- | --- | --- | --- |
| Linux (and macOS) | **Tart** (`tart`) — Apple Virtualization.framework | Native, fast | **Proven loop** — `tart-vm.sh` |
| Windows 11 ARM64 | **QEMU + HVF** (`qemu-system-aarch64 -accel hvf`) | Native arm64 kernel | **Install blocked** — see below; runbook in `~/winvm-build/` |

**Why two tools:** Apple's Virtualization.framework (and therefore Tart, and UTM's "Apple" backend) **cannot boot Windows on ARM** — no Windows-compatible UEFI/virtio guest support (cirruslabs/tart#1123). So Linux uses the fast native path (Tart) and Windows requires QEMU under the Hypervisor.framework accelerator. UTM's `utmctl` only does lifecycle of an *already-built* VM; it doesn't help with the hard part (unattended create + OOBE bypass).

## Linux — the proven loop (`tart-vm.sh`)

Requires Tart (`brew install cirruslabs/cli/tart`) — already installed on this host. `sshpass` (optional, `brew install sshpass`) enables `run` to scp a script in; without it, `run` pipes the script body through `tart exec` stdin.

```bash
# Full demo: clone → boot headless → exec → run a local probe → teardown.
./tart-vm.sh demo

# Or step by step:
./tart-vm.sh up                              # clone ghcr.io/cirruslabs/ubuntu:latest + boot, prints IP
./tart-vm.sh exec -- bash -lc 'uname -srm'   # run a command in the guest
./tart-vm.sh run ./my-probe.sh               # copy a local script in and run it
./tart-vm.sh down                            # stop (disk persists, fast restart)
./tart-vm.sh rm                              # stop + delete (frees disk)
```

Proven end-to-end **2026-06-23** on M1 Max / macOS 26.5 / tart 2.32.1, Ubuntu 24.04.4 arm64. Captured output of `./tart-vm.sh demo`:

```
>> cloning ghcr.io/cirruslabs/ubuntu:latest -> nub-vm-demo-...
>> booting nub-vm-demo-... headless
>> nub-vm-demo-... up at 192.168.64.4
-- exec: guest identity --
ARCH=aarch64
DISTRO=Ubuntu 24.04.4 LTS
WHOAMI=admin
-- run: a local probe script --
probe on: aarch64 / ubuntu
fs write /tmp: ok
egress https: 200
-- teardown --
== demo complete ==
```

Boot-to-IP is ~20s; `tart exec` runs synchronously and returns the guest's stdout/exit code to the host. cirruslabs Linux images log in as `admin`/`admin`. Pass a different image as the last arg to `up` (any `tart`-pullable OCI VM image, e.g. `ghcr.io/cirruslabs/ubuntu:22.04`).

### Worked example — running a sandbox/probe script

`run` is the probe pattern: author a `.sh` on the host, `run` it in the guest, read the result on host stdout. The same shape works for the script-sandbox probes — write the probe (FS write-confine, egress block, read-deny assertions), `run` it, assert on the captured output. For a clean-distro matrix, loop `up <name> <image>` over several images.

## Windows 11 ARM64 — QEMU runbook (install currently BLOCKED)

The full runbook, scripts, and artifacts live in **`~/winvm-build/`** (host home, outside the repo — the ISO is 7.3 GB and the disk image 12 GB). Start with `~/winvm-build/README.md`. It is a complete, well-documented QEMU+HVF setup: EDK2 aarch64 UEFI firmware, an `autounattend.xml` answer file (bypasses TPM/SecureBoot, skips OOBE, creates admin `nub`/`NubWin11!Pass`, installs OpenSSH Server, authorizes `~/.ssh/nub-winvm.pub`, opens firewall :22, PowerShell default shell), NetKVM ARM64 NIC driver injection, NVMe `bootindex=1` boot-order fix, a QMP `kickstart.py` to clear the first-boot UEFI shell, `shot.sh` to screenshot the headless VM, and `install-loop.sh` to relaunch QEMU across Setup's ACPI power-offs. Once SSH is up the VM is driven exactly like Linux — over SSH from the Bash tool:

```bash
ssh -i ~/.ssh/nub-winvm -p 2222 -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null nub@localhost \
    'powershell -NoProfile -Command "$PSVersionTable.OS; $env:PROCESSOR_ARCHITECTURE"'
```

The harness to run once it's up is `../windows/papercut-survey.ps1`.

### Where it's blocked (verified 2026-06-23)

The unattended install reaches **WIM-apply + the first NVMe reboot** (qcow2 grows to ~12-13 GB) then hangs in a **persistent "The computer restarted unexpectedly or encountered an unexpected error. Windows installation cannot proceed." loop at the specialize/OOBE phase** — confirmed by screenshot (TianoCore/EDK2 firmware behind the Windows Setup error dialog). `install-loop.sh` rides out Setup's ACPI power-offs by relaunching from NVMe, but the install never advances past specialize, so SSH on :2222 never comes up. **No verified Windows arm64 SSH output, no clean snapshot.**

Likely cause (per the prior session's analysis in the `windows-papercut` thread): a `RunSynchronousCommand` in the answer file's `specialize`/`oobeSystem` pass returning non-zero (which aborts Setup with exactly this error), and/or the install media staying attached so Setup re-enters WinPE on reboot.

### Human-unblock / next-step options

Diagnosing further needs the Setup logs, which on macOS is the friction point (no NBD to loopback-mount the qcow2 and read `X:\Windows\Panther\setuperr.log`). Concrete unblock paths, cheapest first:

1. **Read the Setup logs.** Convert the qcow2 to raw and mount, or boot the VM with a WinPE/recovery ISO and `type` the `Panther\setupact.log` / `setuperr.log` + `UnattendGC\setupact.log` to find the exact failing specialize step, then fix that one command in `~/winvm-build/unattend/autounattend.xml` and rebuild `unattend.iso`.
2. **Strip the specialize pass.** Move ALL provisioning (OpenSSH install, key authorization, firewall) out of `specialize`'s `RunSynchronousCommand` and into `oobeSystem` `FirstLogonCommands` only, so nothing non-zero can abort Setup mid-specialize. (Prior session's stated fallback plan.)
3. **Do the OOBE once interactively.** Attach a display (VNC is wired: `-vnc 127.0.0.1:1`, connect a VNC client to `localhost:5901`), click through the one stuck transition by hand, then snapshot clean — after that everything is headless/SSH/CLI. This is the "one human step" that most reliably gets to a working baseline.
4. **Recommend-only — a paid VM tool.** VMware Fusion (`VMware Fusion.app` is installed; Fusion is now free for personal use, `vmrun` CLI) and Parallels (`prlctl`, paid) both have first-class Windows-11-ARM support and far smoother unattended Windows installs than hand-rolled QEMU. If a reliable local Windows VM becomes a recurring need, evaluating Fusion's `vmrun`-driven unattended path is the highest-leverage next move — but it's a maintainer call and not required (the `windows-latest` CI leg already covers Windows; this local VM is an iteration-speed convenience, and x64-native is CI-only regardless).

### Platform caveat (state in every Windows claim from this VM)

An ARM VM exercises **win32-arm64 natively** + win32-x64 under Windows-on-ARM's x64 *emulation* layer. **True win32-x64 native is NOT covered** — that stays the `windows-latest` CI leg or an x64 host.

## Hygiene

- Keep VMs ephemeral: `tart-vm.sh rm` (or `demo`, which self-cleans) frees the disk. `tart list` shows what exists; `tart prune` clears caches.
- **Windows: exactly ONE QEMU against `win11.qcow2` at a time** (qcow2 takes a write lock; overlapping launches corrupt install progress — this caused a multi-launch "war" in a prior session). Before any launch: `pkill -9 -f qemu-system-aarch64; sleep 2` must leave no QEMU running.
- Nothing here touches the repo tree or anything destructive on the host; VM disks live under `~/.tart/` (Linux) and `~/winvm-build/` (Windows).
