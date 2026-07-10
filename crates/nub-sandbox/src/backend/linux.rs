//! Linux backend: Landlock (ABI v2) fs read/write-confine + seccomp (net-family
//! deny + ptrace-family deny) + env-scrub. NO bubblewrap, NO user namespaces
//! (they need `CLONE_NEWUSER`, blocked on stock Ubuntu 24.04+/RHEL/CI).
//!
//! THE ENV-READ BOUNDARY (design.md §2.4) rests on two mechanisms here, both
//! inherited across fork/exec and unsheddable under `no_new_privs`:
//!   1. Read-confine NEVER grants `/proc` — so an ascendant's
//!      `/proc/<pid>/environ` cannot be `open()`ed (the PRIMARY, always-available
//!      vector). Enforced in `linux_grants` (the `is_proc_or_sys` hard filter) and
//!      by never adding `/proc` to the essential set.
//!   2. seccomp denies `ptrace` + `process_vm_readv`/`process_vm_writev` — closing
//!      the SECOND vector (scraping an ancestor's memory directly), since host
//!      `yama.ptrace_scope` can't be relied on.
//!
//! Net-deny is likewise inherited + unsheddable, so a recovered secret is inert.
//!
//! WHY EXECUTE IS NOT GOVERNED: `handle_access` covers read (ReadFile|ReadDir) and
//! write, but NOT `AccessFs::Execute`. Landlock's `from_read` bundles Execute, so
//! governing it would confine which binaries run AND (via the loader) require every
//! shared-library dir to be Execute-granted — a fragile way to break dynamic
//! linking for zero security gain here (exec'ing a file never returns its contents,
//! and reads are separately governed). Leaving Execute ungated keeps linking + tool
//! exec robust while read/write confinement holds.
#![cfg(target_os = "linux")]

use crate::backend::linux_grants::{self, DerivedGrants, Grant, GrantKind, fs_confines};
use crate::backend::{CommandSpec, Degradation, Prepared, linux_connect_notify};
use crate::policy::{Effect, SandboxPolicy};
use landlock::{
    ABI, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, NetPort, PathBeneath, PathFd,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// System directories a dynamically-linked child must read to exec + link (loader,
/// libc, toolchain). Deliberately EXCLUDES `/proc`/`/sys` (env-read boundary) and
/// user-data roots (`/home`/`/root`/`/tmp` — those are governed by the policy).
/// The analogue of the macOS SBPL essential base. Nonexistent entries are skipped.
const ESSENTIAL_READ_DIRS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32", "/etc", "/opt",
];

/// Least-privilege `/dev` allowlist (O3) — the device nodes a normal child actually
/// opens, replacing the former wholesale `/dev` rw grant. Each node is granted rw
/// individually; everything else under `/dev` (and `/dev` listing itself) is denied,
/// failing CLOSED (EACCES, visible) rather than leaking. Why each is present:
///   - `null`/`zero`/`full` — the standard sink/source char devices (redirects,
///     `/dev/null` discards, `/dev/zero` fills).
///   - `random`/`urandom` — entropy; libc/openssl/node CSPRNG seeding reads them.
///   - `tty` — the process's controlling terminal (interactive I/O, isatty probes).
///   - `ptmx` + `pts` — the PTY master clone device and the devpts slave directory:
///     a PTY-spawning child (shells under a pseudo-terminal, `node-pty`, `script`)
///     opens `/dev/ptmx` then its allocated `/dev/pts/N`, so the `pts` subtree is
///     granted rw. Absent nodes are skipped by `add_if_exists`, so a host without
///     devpts simply grants fewer nodes.
const DEV_ALLOWLIST: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
];

/// The net enforcement mode chosen for this run (resolved from the policy + whether an
/// egress proxy is running + kernel capability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetMode {
    /// Net unconfined — no socket restriction.
    Off,
    /// Coarse deny-all: no `AF_INET`/`AF_INET6` socket at all (nothing reachable).
    DenyAll,
    /// Per-host via the loopback proxy on `port`. `AF_INET` STREAM sockets are allowed
    /// (so the child can reach the loopback proxy) but narrowed hard: seccomp denies
    /// AF_INET datagram/raw types (no UDP → no DNS-tunnel/QUIC exfil) AND non-TCP stream
    /// protocols (SCTP/MPTCP, which Landlock's IPPROTO_TCP-only `ConnectTcp` would not
    /// govern), Landlock ABI-v4 `ConnectTcp` pins `connect()` to this port, and `BindTcp`
    /// denies explicit `bind()` (a bind-less `listen()` autobind is a dominated residual —
    /// see [`apply_landlock`]).
    ///
    /// The PORT-scoped `ConnectTcp` residual (Landlock cannot filter by address, so a
    /// direct connect to an EXTERNAL host on this port would skip the proxy) is CLOSED by
    /// the seccomp `USER_NOTIF` `connect()` supervisor in [`linux_connect_notify`], which
    /// permits only `127.0.0.1:<port>`; its TCP-Fast-Open twin (`sendto(MSG_FASTOPEN)`,
    /// which connects without `connect()`) is closed by the `MSG_FASTOPEN` deny in
    /// [`build_seccomp`]. The one narrow residual left is on yama `ptrace_scope >= 2`
    /// hosts, where the supervisor cannot read the child's memory and is skipped (the
    /// bounded port-scoped bypass then remains). macOS (address+port carve) has no gap.
    Proxy(u16),
}

/// Apply a resolved policy on Linux. Env-scrub is construction (parent-side); fs is
/// Landlock, net + ptrace are seccomp (+ Landlock-v4 TCP for the per-host proxy), all
/// installed in a `pre_exec` hook. The [`Degradation`] is computed parent-side
/// (capability probe + carve honesty); a hard child-side enforcement failure fails the
/// SPAWN (never runs unconfined).
pub fn apply(
    policy: &SandboxPolicy,
    spec: CommandSpec,
    proxy_port: Option<u16>,
    proxy_token: Option<&str>,
    ca_bundle: Option<&std::path::Path>,
) -> Result<Prepared, Degradation> {
    let mut command = base_command(&spec, policy);
    if let Some(port) = proxy_port {
        super::set_proxy_env(&mut command, port, proxy_token);
    }
    // CA trust for the child (the Landlock read grant is added to the fs ruleset below).
    if let Some(bundle) = ca_bundle {
        super::set_ca_env(&mut command, bundle);
    }

    let confine_fs = fs_confines(&policy.fs);
    let sandboxing = confine_fs || policy.net.enforce || policy.env.enforce;
    if !sandboxing {
        // Fully relaxed + no env scrub — nothing to enforce (mirrors macOS).
        return Ok(Prepared {
            command,
            degradation: Degradation::full(),
            proxy: None,
            connect_notify: None,
        });
    }

    let mut deg = Degradation::full();
    let mut reason: Option<String> = None;

    // Resolve the net mode up front — it drives BOTH the seccomp filter and whether
    // Landlock must handle the TCP-connect access type.
    let net_mode = decide_net_mode(policy, proxy_port, &mut deg, &mut reason);

    // ── fs → Landlock grants (parent-side derivation + PathFd targets) ──────────
    // Landlock engages for a fs-confining policy OR an env-scrub that actually
    // WITHHOLDS something: the env-read boundary requires `/proc` be unreadable
    // whenever the child is denied a var the ancestor holds (else it recovers it via
    // `/proc/<ppid>/environ`), and seccomp cannot close a FILE read — only Landlock
    // (not granting `/proc`) can. An `{env:true}` passthrough withholds nothing, so
    // it needs no `/proc` close (leaving `/proc/cpuinfo` etc. readable for a config
    // the user chose to be permissive).
    let landlock_ok = landlock_available();
    let scrub_withholds = policy.env.enforce && !policy.env.withheld.is_empty();
    // fs Landlock is needed to confine reads/writes OR to close `/proc` for the
    // env-read boundary; net Landlock (ABI v4 ConnectTcp) is needed to pin egress to
    // the proxy port. They compose in ONE ruleset (handle_access ORs fs + net).
    let fs_handling = confine_fs || scrub_withholds;
    let connect_tcp_port = match net_mode {
        NetMode::Proxy(port) => Some(port),
        _ => None,
    };
    let net_handling = connect_tcp_port.is_some();
    let install_landlock = (fs_handling || net_handling) && landlock_ok;
    let mut grant_specs: Vec<(PathBuf, BitFlags<AccessFs>)> = Vec::new();
    let read_bits = read_access_bits();
    let rw_bits = read_bits | AccessFs::from_write(ABI::V2);

    if fs_handling && landlock_ok {
        if confine_fs {
            let DerivedGrants {
                grants,
                mut read_partial,
            } = linux_grants::derive_read_grants(policy);
            for g in grants {
                push_grant(&mut grant_specs, g, read_bits, rw_bits);
            }
            // Write grants: pre-create the target so Landlock can open it (granting
            // the nearest existing ancestor instead would over-grant), then request
            // rw on the subtree. NOTE: pre_create makes a not-yet-existing target a
            // DIRECTORY — a write grant to a not-yet-existing FILE should instead be
            // expressed as its containing directory (a Landlock-vs-Seatbelt limit).
            let (write_grants, write_partial) = linux_grants::derive_write_grants(policy);
            for g in write_grants {
                pre_create(&g.path);
                grant_specs.push((g.path, rw_bits));
            }
            // Essential system read set + /dev (rw for /dev/null redirects) + the
            // program file, so a dynamically-linked child can exec, link, and
            // redirect under read-confine. Granted WHOLESALE for the loader in the
            // common case — but when an explicit policy deny lands INSIDE one (e.g.
            // `!/etc/secret`) the dir is CARVED instead (implicit-allow walk excluding
            // the denied path), so a secret placed under `/etc` is not silently
            // readable while the loader's own files stay granted.
            for dir in ESSENTIAL_READ_DIRS {
                let p = Path::new(dir);
                if !p.exists() {
                    continue;
                }
                if linux_grants::essential_dir_needs_carve(policy, p) {
                    let DerivedGrants {
                        grants: carved,
                        read_partial: carve_partial,
                    } = linux_grants::derive_essential_dir_carve(policy, p);
                    for g in carved {
                        push_grant(&mut grant_specs, g, read_bits, rw_bits);
                    }
                    read_partial |= carve_partial;
                } else {
                    add_if_exists(&mut grant_specs, p, read_bits);
                }
            }
            // /dev is granted per-node, NOT wholesale (O3): the least-privilege set a
            // normal child needs (see [`DEV_ALLOWLIST`]). Landlock does no
            // directory-traverse check, so a leaf grant suffices to open the node by
            // path without granting the /dev directory itself.
            for node in DEV_ALLOWLIST {
                add_if_exists(&mut grant_specs, Path::new(node), rw_bits);
            }
            if let Some(prog) = resolve_program(&spec.program, spec.cwd.as_deref()) {
                add_if_exists(&mut grant_specs, &prog, AccessFs::ReadFile.into());
            }
            // The child must READ the CA bundle to trust the minted leaves — grant it
            // explicitly so a confining fs policy can't hide nub's own trust infra.
            if let Some(bundle) = ca_bundle {
                add_if_exists(&mut grant_specs, bundle, AccessFs::ReadFile.into());
            }
            if read_partial {
                deg.lost.push("fs-read-partial".to_string());
                reason.get_or_insert_with(|| {
                    "read allow-set too large to fully enumerate under a deny — remainder denied"
                        .to_string()
                });
            }
            if write_partial {
                deg.lost.push("fs-write-partial".to_string());
                reason.get_or_insert_with(|| {
                    "write allow-set too large to fully enumerate under a deny — remainder denied"
                        .to_string()
                });
            }
        } else {
            // env-scrub only, fs relaxed: keep fs effectively relaxed (grant rw to
            // every top-level of `/`) but CLOSE `/proc`,`/sys` so the env-read
            // boundary holds. Consequence: a child that reads `/proc/self/*` under a
            // pure env-scrub is denied — the same trade the fs-confine path makes.
            for top in linux_grants::relaxed_top_levels_except_proc_sys() {
                grant_specs.push((top, rw_bits));
            }
        }
    } else if fs_handling {
        // No Landlock (kernel <5.19 / disabled / LinuxKit): do NOT install a fs hook.
        // seccomp (incl. ptrace-family deny) + env-scrub still apply. Report honestly
        // so the caller (build-jail = fail-closed; runtime = fail-open) decides — a
        // pure env-scrub additionally loses the `/proc` ancestor-env close.
        if confine_fs {
            deg.lost.push("fs".to_string());
            reason.get_or_insert_with(|| "Landlock unavailable on this kernel".to_string());
        } else {
            deg.lost.push("env-read-boundary".to_string());
            reason.get_or_insert_with(|| {
                "Landlock unavailable — /proc ancestor-env read not blocked (env scrub still applied)"
                    .to_string()
            });
        }
    }

    deg.reason = reason;

    // ── the child-side hook: NNP → Landlock → seccomp ───────────────────────────
    // Precompute the seccomp BPF parent-side (pure byte assembly, no syscalls) so
    // the post-fork child only calls apply_filter. ptrace-family deny is unconditional
    // when sandboxing; the net socket-family deny follows `net_mode` (Proxy keeps
    // AF_INET so the child can reach the proxy, Landlock pins its connect port).
    let seccomp = build_seccomp(net_mode).map_err(|e| Degradation {
        lost: vec!["seccomp".to_string()],
        reason: Some(e),
    })?;
    // Companion filter that forces glibc off the unfilterable `clone3` onto the
    // flag-filtered `clone` (O1 — see [`build_clone3_enosys`]). Installed alongside
    // the main filter; the kernel takes the most-restrictive verdict per syscall.
    let clone3_enosys = build_clone3_enosys().map_err(|e| Degradation {
        lost: vec!["seccomp".to_string()],
        reason: Some(e),
    })?;

    // ── connect-notify supervisor (Proxy mode only) ─────────────────────────────
    // Close the port-scoped ConnectTcp residual: the child's pre_exec installs a 2nd
    // seccomp filter (connect→USER_NOTIF) and hands its listener fd back over a
    // pre-created socketpair; the parent runs a supervisor that permits only the
    // loopback proxy. `handoff_fds` (Copy) rides into the closure; the parent end +
    // proxy port ride on `Prepared` for the run. Skipped where non-viable (arch /
    // yama ptrace_scope ≥ 2 / a socketpair error) — the documented bounded residual
    // then remains, per-host egress unaffected.
    let mut connect_notify: Option<linux_connect_notify::ConnectNotify> = None;
    let mut handoff_fds: Option<(RawFd, RawFd)> = None;
    if let NetMode::Proxy(port) = net_mode
        && linux_connect_notify::viable()
        && let Ok((parent_sock, child_sock)) = linux_connect_notify::make_socketpair()
    {
        handoff_fds = Some((child_sock.as_raw_fd(), parent_sock.as_raw_fd()));
        connect_notify = Some(linux_connect_notify::ConnectNotify::new(
            parent_sock,
            child_sock,
            port,
        ));
    }

    // SAFETY: the closure runs post-fork/pre-exec. nub may host a tokio runtime, so
    // strictly only async-signal-safe work is sound here; in practice the child is
    // single-threaded post-fork and this open/landlock/seccomp sequence is the
    // proven pattern (ported from the salvage e2e). We keep it minimal: no new
    // threads, no I/O beyond opening the grant PathFds.
    unsafe {
        command.pre_exec(move || {
            // NNP first — REQUIRED before BOTH Landlock restrict_self AND seccomp
            // apply_filter. Set unconditionally so the seccomp-only tier (no
            // Landlock) doesn't EINVAL at apply_filter.
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if install_landlock {
                apply_landlock(fs_handling, &grant_specs, connect_tcp_port)
                    .map_err(std::io::Error::other)?;
            }
            seccompiler::apply_filter(&seccomp).map_err(std::io::Error::other)?;
            if let Some(prog) = &clone3_enosys {
                seccompiler::apply_filter(prog).map_err(std::io::Error::other)?;
            }
            // Install the connect→USER_NOTIF filter LAST and hand its listener fd to the
            // parent. After the EPERM filter (which allows seccomp/sendmsg/close), so the
            // handoff syscalls flow; connect gets USER_NOTIF (lower RET value than the
            // EPERM filter's ALLOW → wins), foreign-ABI stays KILLed by the EPERM filter.
            if let Some((child_raw, parent_raw)) = handoff_fds {
                linux_connect_notify::install_and_handoff(child_raw, parent_raw)?;
            }
            Ok(())
        });
    }

    Ok(Prepared {
        command,
        degradation: deg,
        proxy: None,
        connect_notify,
    })
}

/// Resolve the net enforcement mode. Per-host requires BOTH a running proxy AND
/// Landlock ABI v4 (to pin the child's `connect()` to the proxy port); absent either,
/// we FALL BACK to coarse deny-all (fail-SAFE — denies more, not less) and report
/// `net-per-host` so the caller knows the per-host allows were not honored.
fn decide_net_mode(
    policy: &SandboxPolicy,
    proxy_port: Option<u16>,
    deg: &mut Degradation,
    reason: &mut Option<String>,
) -> NetMode {
    if !policy.net.enforce {
        return NetMode::Off;
    }
    let has_allow = policy.net.rules.iter().any(|r| r.effect == Effect::Allow);
    if !has_allow {
        // Pure deny-all — coarse, no proxy needed.
        return NetMode::DenyAll;
    }
    match proxy_port {
        Some(port) if landlock_net_available() => NetMode::Proxy(port),
        other => {
            deg.lost.push("net-per-host".to_string());
            reason.get_or_insert_with(|| {
                if other.is_none() {
                    "egress proxy unavailable — per-host allows denied (coarse network deny)"
                        .to_string()
                } else {
                    "Landlock TCP rules (ABI v4) unavailable — cannot pin egress to the proxy; \
                     per-host allows denied (coarse network deny)"
                        .to_string()
                }
            });
            NetMode::DenyAll
        }
    }
}

/// The unwrapped command with env-scrub by construction (identical contract to the
/// macOS/skeleton path): when env enforces, the child env IS exactly the policy's
/// constructed map — a withheld secret is simply absent.
fn base_command(spec: &CommandSpec, policy: &SandboxPolicy) -> Command {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if policy.env.enforce {
        command.env_clear();
        for (k, v) in &policy.env.constructed {
            command.env(k, v);
        }
    }
    command
}

/// Read access bits WITHOUT Execute (see the module header): ReadFile | ReadDir.
fn read_access_bits() -> BitFlags<AccessFs> {
    AccessFs::from_read(ABI::V2) & !BitFlags::from(AccessFs::Execute)
}

/// Map a derived [`Grant`] to its `(path, bits)` PathFd spec.
fn push_grant(
    out: &mut Vec<(PathBuf, BitFlags<AccessFs>)>,
    g: Grant,
    read_bits: BitFlags<AccessFs>,
    rw_bits: BitFlags<AccessFs>,
) {
    let bits = match g.kind {
        GrantKind::ReadSubtree => read_bits,
        GrantKind::ReadDir => AccessFs::ReadDir.into(),
        GrantKind::ReadFile => AccessFs::ReadFile.into(),
        GrantKind::WriteSubtree => rw_bits,
    };
    add_if_exists(out, &g.path, bits);
}

/// Add a grant only if the path exists (Landlock can't open a non-existent path;
/// PathFd would Err). Write roots are pre-created by the caller before this.
fn add_if_exists(
    out: &mut Vec<(PathBuf, BitFlags<AccessFs>)>,
    path: &Path,
    bits: BitFlags<AccessFs>,
) {
    if path.exists() {
        out.push((path.to_path_buf(), bits));
    }
}

/// Pre-create a write-grant target so Landlock can grant it directly (a subtree
/// grant of the nearest existing ancestor would over-grant write). Best-effort: a
/// failure just leaves the path absent, and `add_if_exists` skips it (fail-safe —
/// the write stays denied rather than over-granted).
fn pre_create(path: &Path) {
    if !path.exists() {
        let _ = std::fs::create_dir_all(path);
    }
}

/// Build the Landlock ruleset and enforce it on the current (post-fork) task.
/// `handle_fs` engages fs read/write confinement over `specs`; `connect_tcp_port`
/// engages ABI-v4 `ConnectTcp` pinning to that single port (the per-host proxy). The
/// two access types compose in one ruleset (handle_access ORs them). `BestEffort`
/// degrades gracefully on older ABIs; a `NotEnforced` result is a hard error (the
/// parent PROMISED enforcement in the degradation report, so failing the spawn is
/// safer than running unconfined). Net is only requested when the parent already
/// confirmed ABI v4 ([`decide_net_mode`]), so its rule cannot silently vanish.
fn apply_landlock(
    handle_fs: bool,
    specs: &[(PathBuf, BitFlags<AccessFs>)],
    connect_tcp_port: Option<u16>,
) -> Result<(), String> {
    let mut ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    if handle_fs {
        let read = AccessFs::from_read(ABI::V2) & !BitFlags::from(AccessFs::Execute);
        let handled = read | AccessFs::from_write(ABI::V2);
        ruleset = ruleset
            .handle_access(handled)
            .map_err(|e| format!("handle_access fs: {e}"))?;
    }
    if connect_tcp_port.is_some() {
        // Handle BOTH TCP rights: ConnectTcp (allow ONLY the proxy port, below) and
        // BindTcp with NO allowed port — denying every explicit TCP bind(), which
        // removes the deliberate inbound-listener exfil channel. An outbound connect()'s
        // implicit source-port autobind is NOT a bind() and stays permitted, so the
        // proxy path is unaffected. NOTE: Landlock hooks bind + connect but NOT listen,
        // so a bind-LESS listen() still autobinds an ephemeral port — an inbound listener
        // on a RANDOM port remains creatable. That residual is strictly dominated by the
        // port-scoped ConnectTcp residual (below): it needs inbound host reachability AND
        // an out-of-band channel to signal the random port, which the child lacks.
        ruleset = ruleset
            .handle_access(AccessNet::ConnectTcp)
            .map_err(|e| format!("handle_access connect: {e}"))?;
        ruleset = ruleset
            .handle_access(AccessNet::BindTcp)
            .map_err(|e| format!("handle_access bind: {e}"))?;
    }
    let mut created = ruleset
        .create()
        .map_err(|e| format!("create ruleset: {e}"))?;
    for (path, bits) in specs {
        // A path that vanished between derivation and here is skipped, not fatal.
        let Ok(fd) = PathFd::new(path) else { continue };
        created = created
            .add_rule(PathBeneath::new(fd, *bits))
            .map_err(|e| format!("add_rule {}: {e}", path.display()))?;
    }
    if let Some(port) = connect_tcp_port {
        // ConnectTcp allowed to the proxy port only; no BindTcp rule → all binds denied.
        created = created
            .add_rule(NetPort::new(port, AccessNet::ConnectTcp))
            .map_err(|e| format!("add_rule net port {port}: {e}"))?;
    }
    let status = created
        .restrict_self()
        .map_err(|e| format!("restrict_self: {e}"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err("Landlock reported NotEnforced".to_string());
    }
    Ok(())
}

/// The kernel's supported Landlock ABI version via the raw
/// `landlock_create_ruleset(NULL, 0, VERSION)` query — allocation-free and fork-free
/// (safe under a tokio runtime), and immune to `Ruleset::create`'s degrade-to-dummy
/// false positive. A no-Landlock kernel returns `-EOPNOTSUPP`/`-ENOSYS` (< 1).
fn landlock_abi() -> i64 {
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
    // SAFETY: the documented ABI-query form — NULL attr, size 0, VERSION flag —
    // allocates nothing and only reads the supported version number.
    unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

/// `>= v2` means our fs read/write policy can enforce.
fn landlock_available() -> bool {
    landlock_abi() >= ABI::V2 as i64
}

/// `>= v4` means TCP `connect`/`bind` port rules are available — the prerequisite for
/// forcing the child's egress through the loopback proxy (else per-host degrades).
fn landlock_net_available() -> bool {
    landlock_abi() >= ABI::V4 as i64
}

/// Build the seccomp filter for `net_mode`. `ptrace`/`process_vm_readv`/
/// `process_vm_writev` are denied unconditionally (the env-read second vector; an
/// empty rule-vec matches the syscall regardless of args → EPERM).
///
/// Net socket-family deny (when net is enforced):
///   - `AF_UNIX` is denied at `socket()` — closing the local-IPC EGRESS channel
///     (`/var/run/docker.sock` = root-equivalent, abstract-namespace sockets). Node's
///     fork IPC is UNAFFECTED: it uses `socketpair()` (an unnamed connected pair that
///     can't reach a filesystem/abstract socket), which is NOT in the socket() list.
///   - `AF_INET6` + the exotic families are always denied at `socket()`.
///   - `AF_INET`: denied in `DenyAll` (coarse — no egress at all); ALLOWED in `Proxy`
///     mode so the child can reach the loopback proxy, with a Landlock-v4 `ConnectTcp`
///     rule pinning its `connect()` to the proxy port (installed separately).
///   - `io_uring_setup` is denied whenever net is confined (io_uring can create+connect
///     a socket without `socket()`/`connect()`, dodging both this filter and Landlock).
///
/// seccompiler emits `SECCOMP_RET_KILL_PROCESS` on a foreign-ABI (e.g. i386) syscall,
/// so a compat-ABI syscall can't slip past the single-arch filter.
fn build_seccomp(net_mode: NetMode) -> Result<BpfProgram, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|e| format!("unsupported arch for seccomp: {e}"))?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Unconditional deny set (empty rule-vec → always matches → EPERM). Three
    // defense-in-depth families, all closed at the syscall boundary whenever ANY
    // axis is sandboxed:
    //   - ptrace family (the env-read second vector — scraping an ancestor's memory).
    //   - userns + mount family (O1): an unprivileged user namespace + bind-mount
    //     under a granted dir is the strongest FS-confinement escape. Today it's
    //     blocked only by a CHAIN (Landlock never grants /proc → uid_map write fails,
    //     the kernel refuses a mount from an unmapped userns, host AppArmor). Denying
    //     the namespace/mount syscalls here makes FS confinement self-sufficient. The
    //     register-flag `clone` (below) + the `clone3` ENOSYS filter close the two
    //     clone forms; `unshare` and the mount/new-mount-API calls are denied whole.
    //     Normal children (node/sh/python) call none of these.
    //   - pidfd_getfd (O2): the fd-theft primitive — steal a parent's open fd via a
    //     pidfd (needs PTRACE_MODE_ATTACH; zero legit confined use). `pidfd_open`
    //     stays ALLOWED (legit self-child signaling: pidfd_send_signal/waitid).
    #[allow(clippy::useless_conversion)]
    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_unshare,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsmount,
        libc::SYS_fsconfig,
        libc::SYS_fspick,
        libc::SYS_mount_setattr,
        libc::SYS_open_tree,
        libc::SYS_pidfd_getfd,
    ]
    .map(i64::from)
    {
        rules.insert(syscall, Vec::new());
    }

    // clone(CLONE_NEWUSER|CLONE_NEWNS): filter the flags REGISTER (arg0). A plain
    // fork/thread-creating clone sets neither bit, so it still flows; only a
    // namespace-creating clone is denied. The `clone3` form carries its flags behind
    // a POINTER (unfilterable by seccomp) and is handled by a separate ENOSYS filter
    // (see [`build_clone3_enosys`]) that forces glibc's fallback onto this `clone`.
    rules.insert(i64_from(libc::SYS_clone), clone_userns_newns_rules()?);

    if net_mode != NetMode::Off {
        const EXOTIC: [libc::c_int; 11] = [
            libc::AF_NETLINK,
            libc::AF_PACKET,
            libc::AF_VSOCK,
            libc::AF_XDP,
            libc::AF_ALG,
            libc::AF_BLUETOOTH,
            libc::AF_RDS,
            libc::AF_CAN,
            libc::AF_TIPC,
            libc::AF_IB,
            libc::AF_NFC,
        ];
        // socket() deny families. AF_UNIX + AF_INET6 + exotics always; AF_INET only in
        // coarse deny-all (Proxy keeps it so the child can reach the loopback proxy).
        let mut socket_deny = vec![libc::AF_UNIX, libc::AF_INET6];
        socket_deny.extend_from_slice(&EXOTIC);
        if net_mode == NetMode::DenyAll {
            socket_deny.push(libc::AF_INET);
        }
        let mut socket_rules = family_rules(&socket_deny)?;
        if let NetMode::Proxy(_) = net_mode {
            // Proxy mode keeps AF_INET, but the proxy is TCP-only — a UDP (or raw/other)
            // AF_INET socket would dodge BOTH the seccomp family filter AND Landlock's
            // TCP-only ConnectTcp rule, giving unrestricted datagram egress (DNS-tunnel,
            // QUIC, arbitrary UDP C2). So AF_INET is narrowed to SOCK_STREAM only.
            socket_rules.extend(af_inet_non_stream_rules()?);
            // AND to IPPROTO_TCP only: Landlock's ConnectTcp governs ONLY IPPROTO_TCP, so an
            // AF_INET SOCK_STREAM socket over SCTP(132) or MPTCP(262) passes the type
            // narrowing yet dodges the connect hook — arbitrary egress (MPTCP is default-on
            // and transparently falls back to TCP against any server, a drop-in bypass).
            socket_rules.push(af_inet_non_tcp_protocol_rule()?);
        }
        rules.insert(i64_from(libc::SYS_socket), socket_rules);

        // socketpair() deny families — NEVER AF_UNIX (node fork IPC rides it); the
        // inet/exotic families can't make a socketpair anyway, so this is belt-and-braces.
        let mut sp_deny = vec![libc::AF_INET, libc::AF_INET6];
        sp_deny.extend_from_slice(&EXOTIC);
        rules.insert(i64_from(libc::SYS_socketpair), family_rules(&sp_deny)?);

        // io_uring bypasses socket()/connect() entirely — deny its setup when confined.
        rules.insert(i64_from(libc::SYS_io_uring_setup), Vec::new());

        if let NetMode::Proxy(_) = net_mode {
            // TCP Fast Open initiates a connection via sendto/sendmsg(MSG_FASTOPEN)
            // WITHOUT calling connect(), dodging BOTH the connect-notify supervisor and
            // Landlock's connect-only ConnectTcp hook — a direct external-egress bypass on
            // the proxy port (VM-verified: reaches an external host). Deny the flag on
            // every send syscall; legit proxy traffic uses a plain connect()+write, never
            // TFO. `flags` arg index: sendto/sendmmsg = arg3, sendmsg = arg2.
            for (nr, flags_arg) in [
                (i64_from(libc::SYS_sendto), 3u8),
                (i64_from(libc::SYS_sendmsg), 2u8),
                (i64_from(libc::SYS_sendmmsg), 3u8),
            ] {
                rules.insert(nr, vec![msg_fastopen_rule(flags_arg)?]);
            }
        }
    }

    // mismatch_action = Allow (unlisted syscalls flow); match_action = EPERM.
    SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| format!("seccomp build: {e}"))?
    .try_into()
    .map_err(|e| format!("seccomp compile: {e}"))
}

/// Deny a namespace-creating `clone`: one rule per unprivileged-escape flag bit,
/// matching `(flags & bit) == bit` on the flags register (arg0). `CLONE_NEWUSER`
/// (0x1000_0000) is the unprivileged-userns primitive that unlocks the mount escape;
/// `CLONE_NEWNS` (0x0002_0000) is the mount-namespace bit. A normal fork/thread clone
/// sets neither, so this never touches legitimate process/thread creation.
fn clone_userns_newns_rules() -> Result<Vec<SeccompRule>, String> {
    const CLONE_NEWNS: u64 = 0x0002_0000;
    const CLONE_NEWUSER: u64 = 0x1000_0000;
    let mut out = Vec::with_capacity(2);
    for bit in [CLONE_NEWUSER, CLONE_NEWNS] {
        out.push(
            SeccompRule::new(vec![
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(bit), bit)
                    .map_err(|e| format!("clone flag cond: {e}"))?,
            ])
            .map_err(|e| format!("clone flag rule: {e}"))?,
        );
    }
    Ok(out)
}

/// A SEPARATE seccomp filter that returns `ENOSYS` for `clone3` (whenever the main
/// filter is installed). `clone3` carries its flags behind a POINTER, so seccomp
/// cannot inspect `CLONE_NEWUSER`/`CLONE_NEWNS` the way the register-based `clone`
/// filter does. Returning `ENOSYS` (not `EPERM`) makes glibc believe the kernel lacks
/// `clone3` and fall back to the register-based `clone` — which the main filter DOES
/// flag-filter — so a `clone3(CLONE_NEWUSER)` escape is closed without breaking
/// threading (the fallback `clone` for a normal thread carries no namespace bit).
/// This is the same technique Docker's default seccomp profile uses. The `Option` is
/// the extension seam for an arch that exposes no `clone3` number; on the supported
/// x86_64/aarch64 targets `SYS_clone3` always resolves, so this returns `Some`.
fn build_clone3_enosys() -> Result<Option<BpfProgram>, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|e| format!("unsupported arch for seccomp: {e}"))?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    rules.insert(i64_from(libc::SYS_clone3), Vec::new());
    let prog = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        arch,
    )
    .map_err(|e| format!("clone3 seccomp build: {e}"))?
    .try_into()
    .map_err(|e| format!("clone3 seccomp compile: {e}"))?;
    Ok(Some(prog))
}

/// Deny `socket(AF_INET, _, proto)` for any `proto` other than default (0) or
/// `IPPROTO_TCP` (6) — one rule ANDing `family == AF_INET`, `proto != 0`, `proto != TCP`.
/// A whitelist (not an SCTP/MPTCP blacklist) so any future alt stream protocol is caught
/// too. This is the PRIMARY close for the SCTP/MPTCP egress bypass (kills them at
/// creation, and — unlike the connect-notify supervisor — is not viability-gated); the
/// protocol-agnostic `connect()` supervisor is the backstop.
fn af_inet_non_tcp_protocol_rule() -> Result<SeccompRule, String> {
    SeccompRule::new(vec![
        SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_INET as u64,
        )
        .map_err(|e| format!("proto family cond: {e}"))?,
        SeccompCondition::new(2, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, 0)
            .map_err(|e| format!("proto ne-default cond: {e}"))?,
        SeccompCondition::new(
            2,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            libc::IPPROTO_TCP as u64,
        )
        .map_err(|e| format!("proto ne-tcp cond: {e}"))?,
    ])
    .map_err(|e| format!("proto rule: {e}"))
}

/// Deny a send syscall carrying `MSG_FASTOPEN` (TFO's connect-without-`connect()` flag):
/// match `(flags & MSG_FASTOPEN) == MSG_FASTOPEN` on the given arg index. `MSG_FASTOPEN`
/// is `0x2000_0000` (not always surfaced by libc); legit flag combos never set that bit.
fn msg_fastopen_rule(flags_arg: u8) -> Result<SeccompRule, String> {
    const MSG_FASTOPEN: u64 = 0x2000_0000;
    SeccompRule::new(vec![
        SeccompCondition::new(
            flags_arg,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(MSG_FASTOPEN),
            MSG_FASTOPEN,
        )
        .map_err(|e| format!("tfo cond: {e}"))?,
    ])
    .map_err(|e| format!("tfo rule: {e}"))
}

/// One seccomp rule per family, matching `socket(domain == family)` (arg0). An empty
/// return list would match ALL calls, so a caller must never pass an empty slice here.
fn family_rules(families: &[libc::c_int]) -> Result<Vec<SeccompRule>, String> {
    let mut out = Vec::with_capacity(families.len());
    for &f in families {
        out.push(
            SeccompRule::new(vec![
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, f as u64)
                    .map_err(|e| format!("net cond: {e}"))?,
            ])
            .map_err(|e| format!("net rule: {e}"))?,
        );
    }
    Ok(out)
}

/// Deny `socket(AF_INET, <non-stream>, …)`: one rule per non-stream type, each an AND
/// of `arg0 == AF_INET` and `(arg1 & SOCK_TYPE_MASK) == <type>` (the low 4 bits carry
/// the type; `SOCK_NONBLOCK`/`SOCK_CLOEXEC` flags ride the high bits and are masked
/// off). Leaves `SOCK_STREAM` (the proxy's TCP) the only permitted AF_INET socket.
fn af_inet_non_stream_rules() -> Result<Vec<SeccompRule>, String> {
    // SOCK_TYPE_MASK == 0xf on Linux (the base type occupies the low nibble).
    const SOCK_TYPE_MASK: u64 = 0xf;
    // Every AF_INET base type except SOCK_STREAM(1): DGRAM(2), RAW(3), RDM(4),
    // SEQPACKET(5), DCCP(6). RAW needs CAP_NET_RAW (already unavailable to an
    // unprivileged child) but is denied for completeness; DGRAM is the live exfil hole.
    let non_stream: [u64; 5] = [
        libc::SOCK_DGRAM as u64,
        libc::SOCK_RAW as u64,
        libc::SOCK_RDM as u64,
        libc::SOCK_SEQPACKET as u64,
        6, // SOCK_DCCP (not always in libc)
    ];
    let mut out = Vec::with_capacity(non_stream.len());
    for ty in non_stream {
        out.push(
            SeccompRule::new(vec![
                SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    libc::AF_INET as u64,
                )
                .map_err(|e| format!("inet cond: {e}"))?,
                SeccompCondition::new(
                    1,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::MaskedEq(SOCK_TYPE_MASK),
                    ty,
                )
                .map_err(|e| format!("inet type cond: {e}"))?,
            ])
            .map_err(|e| format!("inet type rule: {e}"))?,
        );
    }
    Ok(out)
}

/// Widen a syscall number to the `i64` map key across arches (`c_long` is i32 on
/// 32-bit, i64 on 64-bit) without a clippy `useless_conversion` on 64-bit.
#[allow(clippy::useless_conversion)]
fn i64_from(n: libc::c_long) -> i64 {
    i64::from(n)
}

/// Resolve a program to an absolute path (best-effort). Absolute → itself; a
/// cwd-relative path → joined against the child's cwd; a bare name → PATH search.
fn resolve_program(program: &std::ffi::OsStr, child_cwd: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    if p.components().count() > 1 {
        let base = match child_cwd {
            Some(c) => c.to_path_buf(),
            None => std::env::current_dir().ok()?,
        };
        return Some(base.join(p));
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(p);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bits_exclude_execute_include_read() {
        let b = read_access_bits();
        assert!(b.contains(AccessFs::ReadFile));
        assert!(b.contains(AccessFs::ReadDir));
        assert!(!b.contains(AccessFs::Execute), "execute must stay ungated");
    }

    #[test]
    fn seccomp_builds_for_each_net_mode() {
        // The filter assembles (BPF byte-gen, no syscalls) for every net mode.
        assert!(build_seccomp(NetMode::Off).is_ok());
        assert!(build_seccomp(NetMode::DenyAll).is_ok());
        assert!(build_seccomp(NetMode::Proxy(8080)).is_ok());
    }

    #[test]
    fn family_rules_never_empty() {
        // An empty family list would match ALL socket() calls (empty rule-vec) — guard
        // against a future edit passing an empty slice.
        assert!(!family_rules(&[libc::AF_UNIX]).unwrap().is_empty());
    }
}
