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
use crate::backend::{CommandSpec, Degradation, Prepared};
use crate::policy::{Effect, SandboxPolicy};
use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
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

/// Apply a resolved policy on Linux. Env-scrub is construction (parent-side); fs is
/// Landlock, net + ptrace are seccomp, both installed in a `pre_exec` hook. The
/// [`Degradation`] is computed parent-side (capability probe + carve honesty); a
/// hard child-side enforcement failure fails the SPAWN (never runs unconfined).
pub fn apply(policy: &SandboxPolicy, spec: CommandSpec) -> Result<Prepared, Degradation> {
    let mut command = base_command(&spec, policy);

    let confine_fs = fs_confines(&policy.fs);
    let sandboxing = confine_fs || policy.net.enforce || policy.env.enforce;
    if !sandboxing {
        // Fully relaxed + no env scrub — nothing to enforce (mirrors macOS).
        return Ok(Prepared {
            command,
            degradation: Degradation::full(),
        });
    }

    let mut deg = Degradation::full();
    let mut reason: Option<String> = None;

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
    let want_landlock = confine_fs || scrub_withholds;
    let install_landlock = want_landlock && landlock_ok;
    let mut grant_specs: Vec<(PathBuf, BitFlags<AccessFs>)> = Vec::new();
    let read_bits = read_access_bits();
    let rw_bits = read_bits | AccessFs::from_write(ABI::V2);

    if want_landlock && landlock_ok {
        if confine_fs {
            let DerivedGrants {
                grants,
                read_partial,
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
            // redirect under read-confine. These are granted WHOLESALE for the loader;
            // an explicit policy deny landing INSIDE one (e.g. `!/etc/x`) is not
            // carved out of them (documented limitation — net-deny mitigates exfil).
            for dir in ESSENTIAL_READ_DIRS {
                add_if_exists(&mut grant_specs, Path::new(dir), read_bits);
            }
            add_if_exists(&mut grant_specs, Path::new("/dev"), rw_bits);
            if let Some(prog) = resolve_program(&spec.program, spec.cwd.as_deref()) {
                add_if_exists(&mut grant_specs, &prog, AccessFs::ReadFile.into());
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
    } else if want_landlock {
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

    // ── net → per-host degradation (seccomp is coarse; proxy not wired) ─────────
    if policy.net.enforce && policy.net.rules.iter().any(|r| r.effect == Effect::Allow) {
        deg.lost.push("net-per-host".to_string());
        reason.get_or_insert_with(|| {
            "egress proxy not wired — per-host allows denied (coarse network deny)".to_string()
        });
    }

    deg.reason = reason;

    // ── the child-side hook: NNP → Landlock → seccomp ───────────────────────────
    // Precompute the seccomp BPF parent-side (pure byte assembly, no syscalls) so
    // the post-fork child only calls apply_filter. ptrace-family deny is
    // unconditional when sandboxing; net-family deny is added iff net enforces.
    let seccomp = build_seccomp(policy.net.enforce).map_err(|e| Degradation {
        lost: vec!["seccomp".to_string()],
        reason: Some(e),
    })?;

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
                apply_landlock(&grant_specs).map_err(std::io::Error::other)?;
            }
            seccompiler::apply_filter(&seccomp).map_err(std::io::Error::other)?;
            Ok(())
        });
    }

    Ok(Prepared {
        command,
        degradation: deg,
    })
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

/// Build the Landlock ruleset from pre-derived `(path, bits)` specs and enforce it
/// on the current (post-fork) task. `BestEffort` degrades gracefully on older ABIs;
/// a `NotEnforced` result is a hard error (the parent PROMISED fs enforcement in the
/// degradation report, so failing the spawn is safer than running unconfined).
fn apply_landlock(specs: &[(PathBuf, BitFlags<AccessFs>)]) -> Result<(), String> {
    let read = AccessFs::from_read(ABI::V2) & !BitFlags::from(AccessFs::Execute);
    let handled = read | AccessFs::from_write(ABI::V2);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(handled)
        .map_err(|e| format!("handle_access: {e}"))?
        .create()
        .map_err(|e| format!("create ruleset: {e}"))?;
    for (path, bits) in specs {
        // A path that vanished between derivation and here is skipped, not fatal.
        let Ok(fd) = PathFd::new(path) else { continue };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, *bits))
            .map_err(|e| format!("add_rule {}: {e}", path.display()))?;
    }
    let status = ruleset
        .restrict_self()
        .map_err(|e| format!("restrict_self: {e}"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err("Landlock reported NotEnforced".to_string());
    }
    Ok(())
}

/// Probe the kernel's supported Landlock ABI via the raw
/// `landlock_create_ruleset(NULL, 0, VERSION)` query — allocation-free and
/// fork-free (safe under a tokio runtime), and immune to `Ruleset::create`'s
/// degrade-to-dummy false positive. `>= v2` means our v2 policy can FullyEnforce; a
/// no-Landlock kernel returns `-EOPNOTSUPP`/`-ENOSYS`.
fn landlock_available() -> bool {
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
    // SAFETY: the documented ABI-query form — NULL attr, size 0, VERSION flag —
    // allocates nothing and only reads the supported version number.
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    abi >= ABI::V2 as i64
}

/// Build the seccomp filter. `ptrace`/`process_vm_readv`/`process_vm_writev` are
/// denied unconditionally (the env-read second vector; an empty rule-vec matches the
/// syscall regardless of args → EPERM). When `deny_net`, `AF_INET`/`AF_INET6` (+
/// exotic families, keeping `AF_UNIX` for node IPC) are denied at socket creation
/// AND `io_uring_setup` is denied (io_uring can create a socket without `socket()`)
/// — coarse (seccomp can't inspect a connect() sockaddr), so this denies ALL TCP
/// egress including loopback; the loopback→proxy carve arrives with the proxy phase.
/// seccompiler emits `SECCOMP_RET_KILL_PROCESS` on a foreign-ABI (e.g. i386) syscall,
/// so a compat-ABI syscall can't slip past the single-arch filter.
fn build_seccomp(deny_net: bool) -> Result<BpfProgram, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|e| format!("unsupported arch for seccomp: {e}"))?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Unconditional ptrace-family deny (empty rule-vec → always matches).
    #[allow(clippy::useless_conversion)]
    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
    ]
    .map(i64::from)
    {
        rules.insert(syscall, Vec::new());
    }

    if deny_net {
        let denied = [
            libc::AF_INET,
            libc::AF_INET6,
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
        let mut family_rules = Vec::with_capacity(denied.len());
        for f in denied {
            family_rules.push(
                SeccompRule::new(vec![
                    SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, f as u64)
                        .map_err(|e| format!("net cond: {e}"))?,
                ])
                .map_err(|e| format!("net rule: {e}"))?,
            );
        }
        #[allow(clippy::useless_conversion)]
        for syscall in [libc::SYS_socket, libc::SYS_socketpair].map(i64::from) {
            rules.insert(syscall, family_rules.clone());
        }
        // io_uring can create+connect a socket without ever calling socket()
        // (IORING_OP_SOCKET/CONNECT), bypassing the family filter above. Deny
        // io_uring setup outright when net is confined — no build workload needs it.
        #[allow(clippy::useless_conversion)]
        rules.insert(i64::from(libc::SYS_io_uring_setup), Vec::new());
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
    fn seccomp_builds_for_this_arch() {
        // The filter assembles (BPF byte-gen, no syscalls) with and without net.
        assert!(build_seccomp(true).is_ok());
        assert!(build_seccomp(false).is_ok());
    }
}
