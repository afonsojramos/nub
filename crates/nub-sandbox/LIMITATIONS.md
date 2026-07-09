# nub-sandbox — known limitations

An honest record of what the engine does NOT close, why each residual is bounded, and
where the fix lives. The sandbox never *silently* drops enforcement: every item below
that a policy could reach is also surfaced at runtime via `Degradation` (fail-safe, not
fail-silent). This file is the companion to those runtime signals — the durable
"what's-not-covered" record the final PR and the build-jail thread depend on.

Two kinds of residual appear here:

- **Engine residuals** — a bound the OS primitive itself imposes (Landlock has no
  address filter; an inheritable Windows allow-ACE defeats a nested deny). The engine
  reports these and does not claim them closed.
- **Launcher-handoff items** — the engine constructs the child's confinement correctly,
  but a complete guarantee needs the *launcher* (the future build-jail/embedder that
  owns the parent process and the work-dir layout) to satisfy a contract the
  frontend-less engine cannot. These are NOT engine defects; they define the launcher
  contract.

## Network

### Linux per-host egress: the port-scoped `ConnectTcp` residual (CLOSED via seccomp user_notify)

Under a per-host allowlist the child is forced through the loopback egress proxy, and
Landlock ABI-v4 `ConnectTcp` pins `connect()` to the proxy's port. Landlock has **no
address filter**, so historically a direct TCP `connect()` to an *external* host on the
(random, per-run) proxy port bypassed the per-host gate. **This is now closed** by a
seccomp `USER_NOTIF` supervisor over `connect()`: a filter installed in the child's
pre_exec (raw `seccomp(…, NEW_LISTENER)`, unprivileged under `no_new_privs`) routes every
`connect()` to a notification the nub PARENT services — it reads the child's destination
sockaddr from `/proc/<pid>/mem`, re-validates the notif id, and permits ONLY
`127.0.0.1:<proxy_port>`. On ALLOW the supervisor OWNS the connect (connects a fresh socket
to the FIXED proxy address and injects it over the child's fd via `NOTIF_ADDFD` SETFD),
which makes the allow path TOCTOU-robust against a post-read sockaddr rewrite; DENY returns
EPERM. The TCP-Fast-Open variant — `sendto`/`sendmsg(MSG_FASTOPEN)`, which initiates a
connection *without* `connect()` and so dodges both this filter and Landlock — is closed
alongside it by a seccomp `MSG_FASTOPEN`-flag deny on the send syscalls. See
`backend/linux_connect_notify.rs` and `backend/linux.rs` (`build_seccomp`).

- **Residual bound (narrow, hardened-host only):** the supervisor reads the child's memory
  via `/proc/<pid>/mem`, which yama `ptrace_scope >= 2` forbids even for a parent. On such
  hosts the supervisor is skipped (`viable()` is false) and the pre-existing port-scoped
  `ConnectTcp` residual remains — per-host egress keeps working rather than breaking. The
  default `ptrace_scope <= 1` closes it fully. No capability-floor change: `USER_NOTIF`
  (≥5.0) and `NOTIF_ADDFD` (≥5.9) sit below the Landlock-v4 (6.7) kernel floor proxy mode
  already requires. macOS (address+port carve) and Windows have no equivalent gap.
- **The same supervisor pattern extends to inbound** (`listen()`/`bind()` — the bind-less
  `listen()` autobind residual below): a future hardening slot, not wired now.

### Linux bind-less `listen()` autobind (P3, strictly dominated)

Landlock hooks `bind()` and `connect()` but has **no `socket_listen` hook**, so a
`listen()` issued WITHOUT an explicit `bind()` still autobinds a random ephemeral port —
an inbound listener remains creatable on an unpredictable port. Explicit `bind()`
(including `bind(port = 0)`) IS denied and VM-proven; only the bind-less path is open.

- **Why it adds nothing:** strictly weaker than the `ConnectTcp` residual above — the
  port is unpredictable, the listener needs inbound reachability, and the child has no
  outbound channel to signal the drawn port under net-deny. It confers no capability the
  port-scoped connect edge doesn't already bound; noted only so it is not later mistaken
  for full inbound closure. Same full-close as above (seccomp `user_notify` / netns).
  Documented in `backend/linux.rs` (`apply_landlock`).

### Windows per-host egress is not wired (coarse deny holds)

Windows has proven coarse egress-deny (no `internetClient` capability blocks all egress,
loopback included), but not per-host. An AppContainer child cannot reach the loopback
proxy without a registered loopback exemption (`NetworkIsolationSetAppContainerConfig`),
which this phase does not wire.

- **Why bounded:** fail-safe — a per-host allow policy degrades to coarse **deny**, not
  to open egress, and reports a `net-per-host` `Degradation`. Nothing is silently
  allowed.
- **Where fixed:** wire the loopback exemption so the child can reach the proxy —
  build-jail thread or a later hardening slot.

## Launcher-handoff items (engine correct; launcher must complete the guarantee)

### macOS ascendant-env via `KERN_PROCARGS2`

A sandboxed child can recover a scrubbed secret verbatim by reading the *parent's*
(nub's) argv+environ via `sysctl(KERN_PROCARGS2, getppid())`. This sysctl is not routed
through Seatbelt's `sysctl-read` MACF hook, so Seatbelt cannot block it. The engine
constructs the child's own env correctly (least-privilege); it cannot scrub nub's OWN
environ.

- **Why bounded / status:** the child's own env-scrub holds; only *co-resident same-uid*
  ascendant-env recovery is open. Reported, never claimed closed.
- **Where fixed:** the launcher must not HOLD ambient secrets in nub's own environ when
  it spawns the child (scrub nub's environ pre-spawn, or clean-env re-exec). Launcher
  owns the parent env. See design.md §2.4.

### Windows ascendant-env via same-user `PROCESS_VM_READ`

The Windows twin of the above: a same-user `OpenProcess(PROCESS_VM_READ)` on the parent
reads nub's environ; AppContainer cannot block it. Surfaced as an `env-read-ascendant`
`Degradation` whenever the scrub actually withholds something.

- **Where fixed:** the dedicated-account (distinct low-privilege user) backend, so the
  child cannot open the parent — a post-v0 launcher concern.

### macOS toolchain read-confine for a non-system interpreter

The program auto-grant exposes the program FILE only (never its parent dir — that F3
over-grant is deliberately closed). A non-system Node (Homebrew/nvm) then needs its
toolchain directory in the read-allow set to load its own libraries under a tight
read-confine; the engine does not discover that dir itself (Boundary B — it receives
paths, it does not probe the host).

- **Where fixed:** the launcher/front-end supplies the interpreter's toolchain dir in
  the allow-set. A system interpreter is covered by the essential base and never hits
  this.

### Windows confined work dirs must live under a nub-owned store

A LowBox child does not bypass traverse checking, so every ancestor of a granted leaf
needs an AC-SID traverse grant. Granting traverse on a shared ancestor like
`C:\Users\<user>` needs `WRITE_DAC` on it, which non-elevated nub lacks.

- **Where fixed:** confined work dirs must live under a **nub-owned** store root (whose
  DACL nub controls), so the launcher never needs `WRITE_DAC` on a user/system ancestor.
  A launcher work-dir-layout contract.

### Untrusted-tier tighten-only layering

For the granular object form an omitted axis is currently **relaxed** (the "boolean is
the de-nesting mechanism" contract — you confine what you name). A future *untrusted*
tier wants the opposite default (omitted axis fail-closed / tighten-only), which is a
front-end posture, not an engine mechanism.

- **Where fixed:** the front-end tier that layers tighten-only defaults over the engine.

## Filesystem (bounded P2s, threat-model-mitigated)

Each is documented in code at the site noted; none is silently mis-reported.

- **Linux `/etc` granted wholesale (no deny-inside carve).** Build toolchains read
  `/etc` (resolv.conf, CA bundles), so the generous-read path grants it whole rather
  than per-file. A secret placed *under* `/etc` would be readable. Bounded: user secrets
  do not live in `/etc`, and a USER-authored deny reaching `/etc` still forces a carve.
  Fix: selective `/etc` carve. (`backend/linux_grants.rs`.)
- **Linux write-target widening for a not-yet-existing file.** Landlock cannot grant
  write to a file that does not yet exist, so a write grant for a not-yet-created FILE
  widens to its parent DIR. Bounded over-grant within an already-granted write subtree.
  Fix: none clean at the Landlock layer. (`backend/linux.rs` pre-create.)
- **Linux hardlink-to-secret.** A pre-existing same-uid hardlink to a secret, reached
  via a carve `ReadFile`, bypasses a path-based deny (Landlock keys on the inode reached,
  and the alt path was never denied). Bounded: requires a hardlink created *outside* the
  sandbox beforehand. Fix: none clean at the Landlock layer.
- **Linux derive→open TOCTOU.** Grant derivation canonicalizes paths on the host, then
  the kernel enforces at `open()` later; a path swapped in between could shift a target.
  Bounded: a same-uid local race within the confined tree.
- **Linux bind-mounted procfs at a non-standard path.** The `/proc` filter (the
  ascendant-env boundary) matches the standard mount; a procfs bind-mounted at a
  non-standard path is not matched, re-exposing `/proc/<pid>/environ`. Bounded: requires
  the ability to bind-mount procfs (prior privilege/setup) before entering the sandbox.
  Fix: a mount namespace, or broader procfs detection.
- **Windows program-dir subtree read grant.** The program's parent DIR is auto-granted
  inheritable read so the LowBox child can load sibling DLLs; a project-local tool's
  neighboring files (a `.env` next to a binary) become readable. Bounded for the
  build-jail (the program is the toolchain — e.g. `node.exe` — whose dir holds no user
  secrets). Fix: the front-end owns the program grant explicitly rather than the engine
  auto-widening it. (`backend/windows.rs` `apply`.)
