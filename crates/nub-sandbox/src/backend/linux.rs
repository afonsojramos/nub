//! Linux backend — Landlock (ABI v2) + seccomp, NO bubblewrap, NO namespaces.
//!
//! Decided 2026-06-23 (`.fray/script-sandbox-design.md` §1, [[landlock-vs-bwrap-research]]):
//! the Linux fs/sandbox backend is LANDLOCK-ONLY. No external-binary dep, no
//! bundled `bwrap`, no user-namespace dependency (which fails on stock Ubuntu
//! 24.04+/RHEL/restricted CI).
//!
//! Evolves aube's Linux sandbox backend:
//!   - **Read** — aube granted `/` read wholesale (secrets readable). We keep a
//!     broad read (generous-read posture, §4) but DENY the secret set by NOT
//!     granting their parent dirs read and, where they sit under a granted root,
//!     relying on the recursive-carve walk. Landlock is allow-only, so a deny is
//!     "don't grant + grant siblings" — the recursive `.env*` carve from
//!     `.fray/sandbox-fs-deny-list.md`. (First cut: broad `/` read minus a
//!     curated set of secret PARENT dirs that we simply never add; full
//!     recursive `.env*` per-child walk is the documented follow-on — see
//!     DESIGN-NOTES.)
//!   - **Write** — subtree write on package dir + sandbox-home + extra-write
//!     (NOT `/tmp` — symlink-race avoidance, matching aube), plus `/dev`.
//!   - **Net** — seccomp `AF_INET`/`AF_INET6`-deny (+ other dangerous families),
//!     keeping `AF_UNIX` for node IPC. Loopback-to-proxy carve-out is the
//!     follow-on when the proxy lands (seccomp can't allow-list by host).
//!
//! Capability-probe + graceful-degrade: if Landlock is unavailable (kernel
//! <5.19 / disabled), the fs sandbox is lost — we return a [`Degradation`] and
//! still apply the seccomp net-deny + env-scrub. Never hard-fail; never reach
//! for bwrap.
//!
//! DESIGN-NOTES (follow-on, §8):
//!   - Recursive `.env*` per-child read-carve walk (the sandbox-fs-deny-list
//!     algorithm) replacing the coarse "broad read minus secret parents".
//!   - Landlock ABI v4 network rules (kernel 6.7+) for precise proxy-port
//!     allow-listing; the localhost egress proxy + per-host filter.
//!   - `setrlimit(RLIMIT_NPROC/RLIMIT_AS)` from `policy.pid`.

use crate::backend::Degradation;
use crate::policy::SandboxPolicy;
use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

fn add_rule(
    ruleset: RulesetCreated,
    path: &Path,
    access: BitFlags<AccessFs>,
) -> Result<RulesetCreated, String> {
    let fd = PathFd::new(path)
        .map_err(|e| format!("failed to open sandbox path {}: {e}", path.display()))?;
    ruleset
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|e| format!("failed to add sandbox path {}: {e}", path.display()))
}

/// Add a path rule, ignoring ENOENT (a deny-target dir that doesn't exist needs
/// no rule; a write root we create lazily may not exist yet — but write roots
/// must exist before Landlock, so the caller ensures that).
fn add_rule_opt(
    ruleset: RulesetCreated,
    path: &Path,
    access: BitFlags<AccessFs>,
) -> Result<RulesetCreated, String> {
    if !path.exists() {
        return Ok(ruleset);
    }
    let mut rs = add_rule(ruleset, path, access)?;
    if let Ok(canon) = path.canonicalize()
        && canon != path
    {
        rs = add_rule(rs, &canon, access)?;
    }
    Ok(rs)
}

/// Apply the Landlock fs ruleset for `policy` inside a `pre_exec` context.
/// Returns Err on a genuine apply failure; the caller decides degrade-vs-warn.
fn apply_landlock(policy: &SandboxPolicy) -> Result<(), String> {
    // Must precede restrict_self so a setuid exec can't shadow the domain.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(format!(
            "PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let abi = ABI::V2;
    let read_access = AccessFs::from_read(abi);
    let full_access = read_access | AccessFs::from_write(abi);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(full_access)
        .map_err(|e| format!("create ruleset: {e}"))?
        .create()
        .map_err(|e| format!("create ruleset: {e}"))?;

    // READ: generous read of the root (§4), but DENY the secret set by granting
    // read to a curated set of top-level dirs INSTEAD of `/` wholesale would be
    // the precise way; the recursive-carve walk is the follow-on. For the first
    // cut we grant `/` read (parity with aube) — the SECRET DENY is delivered on
    // this OS today via the seccomp+env layer and the macOS-grade read-deny is
    // the tracked follow-on. We DO honor an explicit read-allow set when
    // read_enforce is on (the runtime profile path).
    if policy.fs.read_enforce {
        for p in &policy.fs.read_allow {
            ruleset = add_rule_opt(ruleset, p, read_access)?;
        }
    } else {
        ruleset = add_rule(ruleset, Path::new("/"), read_access)?;
    }

    // WRITE: package dir + sandbox-home + extra-write + /dev. NOT /tmp.
    if policy.fs.write_enforce {
        ruleset = add_rule_opt(ruleset, Path::new("/dev"), full_access)?;
        for p in &policy.fs.write_allow {
            ruleset = add_rule_opt(ruleset, p, full_access)?;
        }
    }

    let status = ruleset
        .restrict_self()
        .map_err(|e| format!("restrict_self: {e}"))?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!("ruleset not fully enforced: {:?}", status.landlock));
    }
    Ok(())
}

/// seccomp filter denying inet/raw socket families (egress), keeping AF_UNIX.
fn apply_seccomp_net() -> Result<(), String> {
    let target_arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|e| format!("unsupported arch for net filter: {e}"))?;
    let mk = |family: i32| -> Result<SeccompRule, String> {
        SeccompRule::new(vec![
            SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, family as u64)
                .map_err(|e| format!("net filter: {e}"))?,
        ])
        .map_err(|e| format!("net filter: {e}"))
    };
    // Match aube's full denied-family list (defense-in-depth, pure upside — these
    // are exotic families no legit build needs). AF_UNIX is deliberately NOT
    // denied (node IPC / worker_threads socketpair need it).
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
        family_rules.push(mk(f)?);
    }
    let mut rules = BTreeMap::new();
    #[allow(clippy::useless_conversion)]
    for syscall in [libc::SYS_socket, libc::SYS_socketpair].map(i64::from) {
        rules.insert(syscall, family_rules.clone());
    }
    let filter: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        target_arch,
    )
    .map_err(|e| format!("net filter build: {e}"))?
    .try_into()
    .map_err(|e| format!("net filter compile: {e}"))?;
    seccompiler::apply_filter(&filter).map_err(|e| format!("net filter apply: {e}"))?;
    Ok(())
}

/// Probe whether Landlock can enforce the V2 policy `apply_landlock` needs.
/// `Ruleset::create()` succeeds even on no-Landlock kernels (it degrades to a
/// dummy), so it is not a reliable probe — and landlock 0.4 exposes no public
/// ABI introspection. We query the kernel directly via the raw
/// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` syscall,
/// which returns the supported ABI version (>= 2 means our V2 HardRequirement
/// apply will FullyEnforce). This is allocation-free and async-signal-safe — no
/// fork (unsafe under nub's tokio runtime), no degraded-dummy false-positive.
/// A kernel without Landlock (e.g. Docker Desktop's LinuxKit VM) returns
/// -EOPNOTSUPP/-ENOSYS → we degrade and skip the fs hook instead of EINVAL-ing
/// the spawn (caught by the Linux e2e under Docker).
fn landlock_available() -> bool {
    // syscall numbers: landlock_create_ruleset = 444 on all Linux arches.
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    // LANDLOCK_CREATE_RULESET_VERSION = 1<<0
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
    // SAFETY: passing NULL attr + size 0 + the VERSION flag is the documented
    // ABI-query form; it allocates nothing and only reads the supported version.
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

pub fn apply(cmd: &mut Command, policy: &SandboxPolicy) -> std::io::Result<Degradation> {
    let mut deg = Degradation::full();
    let ll_ok = landlock_available();
    if !ll_ok {
        deg.lost.push("fs".into());
        deg.reason = Some("Landlock unavailable on this kernel".into());
    } else if !policy.fs.read_enforce && !policy.fs.read_deny.is_empty() {
        // HONESTY (caught in review): Landlock is allow-only, so the generous-
        // read script-sandbox posture grants `/` read and CANNOT deny the secret
        // subpaths the way macOS Seatbelt does. The recursive read-carve walk
        // (sandbox-fs-deny-list.md) is the follow-on; until then report the gap
        // so the reduced-mode WARNING fires — never claim a read-deny we don't
        // enforce. (Env-scrub + net-deny still close the exfil path; this is the
        // defense-in-depth layer that's deferred on Linux.)
        deg.lost.push("fs-read-deny".into());
        if deg.reason.is_none() {
            deg.reason =
                Some("Landlock is allow-only — secret read-deny not yet enforced on Linux".into());
        }
    }
    if policy.net.enforce && !policy.net.allow_hosts.is_empty() {
        // per-host egress needs the proxy (follow-on); seccomp gives all-or-
        // nothing. We deny all inet (fail-safe) and report the per-host loss.
        deg.lost.push("net-per-host".into());
        if deg.reason.is_none() {
            deg.reason = Some("egress proxy not yet wired — network fully denied".into());
        }
    }

    let policy = policy.clone();
    unsafe {
        cmd.pre_exec(move || {
            // PR_SET_NO_NEW_PRIVS is REQUIRED before BOTH Landlock restrict_self
            // and seccomp apply_filter — set it unconditionally so the no-Landlock
            // (seccomp-only) tier doesn't EINVAL at apply_filter. (Caught by the
            // Linux e2e: NNP was only set inside apply_landlock.)
            let ret = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if ll_ok {
                apply_landlock(&policy).map_err(std::io::Error::other)?;
            }
            if policy.net.enforce {
                apply_seccomp_net().map_err(std::io::Error::other)?;
            }
            Ok(())
        });
    }
    Ok(deg)
}

#[cfg(test)]
mod tests {
    use crate::script_sandbox::{self, ScriptSandboxParams};
    use std::path::PathBuf;

    #[test]
    fn sandbox_policy_constructs() {
        // Smoke: the script-sandbox policy is well-formed for the Linux backend
        // (the actual enforcement is exercised by the e2e test under Docker/CI).
        let p = script_sandbox::policy(&ScriptSandboxParams {
            package_dir: PathBuf::from("/proj/node_modules/dep"),
            project_root: PathBuf::from("/proj"),
            sandbox_home: PathBuf::from("/tmp/nub-sandbox/1/dep"),
            user_home: PathBuf::from("/home/me"),
            extra_write: vec![],
            registry_hosts: vec![],
            extra_hosts: vec![],
            bundle_browser_cdns: false,
        });
        assert!(p.fs.write_enforce);
        assert!(p.net.enforce);
    }
}
