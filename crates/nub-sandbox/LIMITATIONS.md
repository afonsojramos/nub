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

### Egress SSRF: cloud-metadata / link-local blocked; broad RFC1918 is a posture call (partly open by design)

The loopback egress proxy resolves an allowed host and connects to the resolved IP, so an
allowed hostname whose DNS points at an internal address — or an attacker DNS-*rebinding*
an allowed domain to one between validation and connect — could reach an off-limits
address. Two halves:

- **CLOSED — cloud-metadata / link-local + rebinding.** The proxy fails closed at the
  outbound connect on the IMDS / link-local surface: IPv4 `169.254.0.0/16` (incl. the
  `169.254.169.254` metadata endpoint), IPv6 link-local `fe80::/10`, and the AWS IPv6 IMDS
  `fd00:ec2::254` — regardless of what the policy admits. IPv4-in-IPv6 encodings
  (`::ffff:169.254.169.254`, `::169.254.169.254`) are unmapped before classification, and
  integer/octal/hex host forms are moot because classification runs on the RESOLVED
  `IpAddr`, not the child's token. Rebinding is pinned out: the host is resolved exactly
  once and the connect targets that same address — no re-resolution between check and
  connect. See `proxy/mod.rs` (`is_blocked_egress_ip`, `connect_upstream`).
- **OPEN BY DESIGN (maintainer posture call) — broad RFC1918 private ranges.** `10/8`,
  `172.16/12`, and `192.168/16` are NOT blocked by default. Blocking them wholesale breaks
  legitimate private-host allowlisting and collides with the deliberate loopback carve
  (the proxy's own listener and loopback upstreams must stay reachable), so it is a
  separate posture decision rather than folded into this guard. The seam is clean: a
  policy/config toggle can extend `is_blocked_egress_ip` to the private ranges when the
  maintainer decides the default. Until then, private-range egress is admitted iff the
  active policy admits the host.
- **OPEN residual (impractical) — NAT64 / 6to4 IPv6 embeddings of link-local.** A
  link-local address wrapped in the NAT64 well-known prefix (`64:ff9b::169.254.169.254`)
  or 6to4 (`2002:a9fe:a9fe::`) is NOT unwrapped, so it dodges the block. Reaching IMDS this
  way needs a NAT64/6to4 *translating gateway* on-path routing to a link-local target —
  absent in a normal cloud environment — so it is not a practical metadata reach. Left
  unblocked rather than partly-covered because only the well-known prefixes are detectable
  (a network-specific NAT64 `/96` is not), and partial coverage would misrepresent the
  guarantee. Same `is_blocked_egress_ip` seam if the threat model later wants it.

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
EPERM. Two sibling bypasses of the same gate are closed alongside it, both pure seccomp
(so they hold even where the supervisor is skipped, below): (1) the **TCP-Fast-Open**
variant — `sendto`/`sendmsg(MSG_FASTOPEN)`, which initiates a connection *without*
`connect()` — via a `MSG_FASTOPEN`-flag deny on the send syscalls; (2) **non-TCP stream
protocols** — Landlock's `ConnectTcp` governs only `IPPROTO_TCP`, so an `AF_INET`
`SOCK_STREAM` socket over SCTP (`IPPROTO_SCTP`) or MPTCP (`IPPROTO_MPTCP`, which is
default-on and transparently falls back to TCP against any server) would pass the type
narrowing yet dodge the hook — closed by narrowing the `socket()` protocol (arg2) to TCP
only. See `backend/linux_connect_notify.rs` and `backend/linux.rs` (`build_seccomp`).

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

### Windows ascendant-env via same-user `PROCESS_VM_READ` — OS-CLOSED (not a residual)

Previously suspected as the Windows twin of the macOS ascendant-env read; **empirically
disproven** — the AppContainer closes it. A LowBox child CANNOT
`OpenProcess(PROCESS_VM_READ)` the parent to read nub's environ: the AppContainer access
check requires the target process object's DACL to grant the child's package SID, a
capability, or `ALL APPLICATION PACKAGES`, and a normal parent process grants only the user
SID — so the open is denied (`ERROR_ACCESS_DENIED`), **independent of integrity level**.

- **CI-proven on windows-latest (run 29043151805)** with the parent BOTH elevated AND
  de-elevated (Medium-IL standard user): the AppContainer child's `OpenProcess(PROCESS_VM_READ
  | PROCESS_QUERY_LIMITED_INFORMATION)` on the parent is DENIED (exit 5), while an unconfined
  control recovers the secret (exit 0 — negative control proving the read path is live). So
  **no dedicated-account backend is needed for this axis.**
- **Honest bound:** the `PROCESS_VM_READ`-inclusive `OpenProcess` is proven denied; a
  `PROCESS_QUERY_LIMITED_INFORMATION`-only handle was not separately probed, but it cannot
  read the environment block (that requires `PROCESS_VM_READ`), so it does not reopen the axis.
- **VM-reconfirmed (burn box, standard `nub` user, 2026-07):** an AppContainer child's
  `OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION)` AND `(… | QUERY_LIMITED)` on
  its same-user parent are BOTH denied `ERROR_ACCESS_DENIED` — independent of the CI run
  (`tests/windows_ascendant_env.rs`, which mounts the full PEB-walk attack, not just the
  reporting check).
- **Code state:** `backend/windows.rs` `apply` emits NO `env-read-ascendant` `Degradation`
  (the enforcement suite locks that in) — the axis is closed by the OS, not merely reported.

### macOS toolchain read-confine for a non-system interpreter

The program auto-grant exposes the program FILE only (never its parent dir — that F3
over-grant is deliberately closed). A non-system Node (Homebrew/nvm) then needs its
toolchain directory in the read-allow set to load its own libraries under a tight
read-confine; the engine does not discover that dir itself (Boundary B — it receives
paths, it does not probe the host).

- **Where fixed:** the launcher/front-end supplies the interpreter's toolchain dir in
  the allow-set. A system interpreter is covered by the essential base and never hits
  this.

### Windows confined work dirs need a CLEAN-DACL root (not a nub-owned store; not ancestor traverse grants)

Superseded — a LowBox token retains SeChangeNotifyPrivilege (Bypass Traverse Checking) and
standard NTFS volumes carry `FILE_DEVICE_ALLOW_APPCONTAINER_TRAVERSAL`, so intermediate-dir
ACLs are NOT access-checked: a leaf-only AC-SID grant is reachable under an ORDINARY
`%TEMP%`/profile tree with no ancestor traverse grants and no `C:\`-owned store (VM-verified
under `%TEMP%`, `tests/windows_enforcement.rs` + `windows_residuals.rs`). nub never needs
`WRITE_DAC` on a shared ancestor.

- **Real launcher contract:** the confined root must carry a CLEAN DACL — no inherited
  `ALL APPLICATION PACKAGES` allow-ACE. Where a work dir inherits an AAP grant (some
  `%TEMP%`/profile trees), an ungranted secret UNDER it is readable regardless of the
  allow-set (the AAP grant satisfies the LowBox check before default-deny). Demonstrated by
  the `windows_residuals.rs` RT-B probe; the fixtures strip inherited ACEs
  (`icacls /inheritance:r`) to model the clean root the launcher provides.

### Untrusted-tier tighten-only layering

For the granular object form an omitted axis is currently **relaxed** (the "boolean is
the de-nesting mechanism" contract — you confine what you name). A future *untrusted*
tier wants the opposite default (omitted axis fail-closed / tighten-only), which is a
front-end posture, not an engine mechanism.

- **Where fixed:** the front-end tier that layers tighten-only defaults over the engine.

## Filesystem (bounded P2s, threat-model-mitigated)

Each is documented in code at the site noted; none is silently mis-reported.

- **Linux `/etc` granted wholesale for the loader — a user deny now carves it (FIXED).**
  Build toolchains read `/etc` (resolv.conf, CA bundles), so the essential-read set grants
  `/etc` (and `/usr`,`/lib`,…) whole for the dynamic loader. Previously an explicit
  `!/etc/secret` was NOT honored — the wholesale essential-base grant overrode the carve,
  so a secret under `/etc` stayed readable despite the deny (VM-verified: `!/etc/**`,
  `!/etc/secret`, `!/etc/*`, `!/etc` all failed to deny). Now the essential-base grant
  CARVES an essential dir when a user deny reaches inside it (an implicit-allow walk
  excluding the denied path), so the deny is honored while the loader's own files stay
  readable — VM-verified surgical (wholesale vs carved `/etc` differ in exactly the denied
  file; `ld.so.cache`, `ca-certificates.crt`, dynamic linking all unaffected). Residual:
  with NO user deny, `/etc` is still granted whole (a secret under it is readable — user
  secrets do not live in `/etc`, and net-deny blocks exfil). (`backend/linux.rs` +
  `backend/linux_grants.rs` `essential_dir_needs_carve`/`derive_essential_dir_carve`.)
- **Linux write-target for a not-yet-existing file becomes a DIRECTORY (not a parent
  widen).** Landlock cannot grant write to a file that does not yet exist, so the backend
  pre-creates the target and grants that subtree. VM-verified: `pre_create` uses
  `create_dir_all`, so the requested leaf is created as a DIRECTORY and its subtree is
  write-granted — the containing PARENT dir is NOT widened (a sibling in the parent stays
  denied). Consequences: (a) a tool expecting to write a FILE at that path finds a
  directory; (b) the over-grant is the target-as-dir subtree, bounded within the named
  path. Fix: none clean at the Landlock layer. (`backend/linux.rs` `pre_create`.)
- **Linux hardlink-to-secret (VM-verified).** A pre-existing same-uid hardlink to a
  secret, at a name the deny never targets, is granted `ReadFile`, which grants read on
  the SHARED inode — so the path-denied secret is reachable through the alias (and, since
  the grant is inode-keyed, the denied path itself then leaks too). VM-verified: with the
  alias present the secret leaks; without it the path-deny holds. Bounded: requires a
  hardlink created *outside* the sandbox beforehand. Fix: none clean at the Landlock layer
  (the inode was legitimately named twice). Regression test:
  `hardlink_to_denied_secret_leaks_via_alias`.
- **Linux derive→open TOCTOU.** Grant derivation canonicalizes paths on the host, then
  the kernel enforces at `open()` later; a path swapped in between could shift a target.
  Bounded: a same-uid local race within the confined tree. (Inherent to a canonicalize-
  then-enforce split; not deterministically reproducible.)
- **Linux bind-mounted procfs at a non-standard path.** The `/proc` filter (the
  ascendant-env boundary) is path-literal (`starts_with("/proc")`), so a procfs
  bind-mounted at a non-standard path is not filtered and IS grantable when a covering fs
  grant reaches it — VM-verified: under `{fs:["/tmp"]}` a `/proc` bind-mounted at
  `/tmp/altproc` was readable (`/tmp/altproc/version` leaked). The environ-secret read
  itself (`/proc/<pid>/environ`) is additionally gated by `ptrace_may_access`, which held
  in every VM topology tried (ancestor non-dumpable / sibling non-attachable under
  `ptrace_scope>=1`), so the specific ascendant-env leak was not reproduced — but the
  filter bypass is real. Bounded: requires the ability to bind-mount procfs (prior
  privilege/setup) before entering the sandbox. Fix: a mount namespace, or resolve the
  mount type rather than the path prefix.
- **macOS move/rename secret-relocation — literal AND regex directory-pinning denies CLOSED.**
  A write-deny keyed to a secret's path is defeatable by renaming a container dir out from
  under the deny. Both deny shapes now pin the container: a literal `(subpath)` deny pins its
  ancestor-dir chain, and a regex directory-pinning deny (`!secrets/*.key` → `/proj/secrets/*.key`)
  pins its literal directory prefix (`/proj/secrets`) and up to the write-grant root, so
  `mv secrets secretz` can no longer relocate the matched leaves. VM/host-verified: the
  ancestor rename is blocked while a legit write under the pinned dir still succeeds
  (`tests/macos_moveblock.rs`, `emit_move_block` in `backend/macos.rs`). **Residual (bounded):**
  a floating-name deny with no fixed literal prefix (`!**/secrets/**` — the `secrets` component
  can appear at any depth) has no single directory to anchor, so renaming an inner `secrets/`
  dir still relocates it. Bounded — it needs a user-authored floating-glob deny AND a writable
  container; the file-level deny still blocks renaming the leaves themselves.
- **Windows program grant is file-only (neighbor-read leak CLOSED).** The engine grants
  read+execute on the program FILE ITSELF, not its parent dir (traverse-bypass makes the
  leaf-object ACL sufficient to exec), so a `.env` next to a binary is no longer swept
  into the allow-set. Mirrors the macOS file-only program grant. VM-verified: the neighbor
  `.env` is DENIED while the child still execs and reads its granted dirs
  (`tests/windows_residuals.rs` R1, `tests/windows_enforcement.rs`). Residual launcher
  contract (identical to the macOS "toolchain read-confine" item above): a program that
  loads SIBLING DLLs from its own dir needs the front-end to supply that toolchain dir in
  the read allow-set — the engine no longer auto-widens. A self-contained build-jail
  toolchain (`node.exe`) needs nothing more. (`backend/windows.rs` `apply`.)
