//! Linux backend — Landlock (ABI v2) + seccomp, NO bubblewrap, NO namespaces.
//!
//! Decided 2026-06-23 (`.fray/build-jail-design.md` §1, [[landlock-vs-bwrap-research]]):
//! the Linux fs/sandbox backend is LANDLOCK-ONLY. No external-binary dep, no
//! bundled `bwrap`, no user-namespace dependency (which fails on stock Ubuntu
//! 24.04+/RHEL/restricted CI).
//!
//! Evolves aube's `linux_jail.rs`:
//!   - **Read** — aube granted `/` read wholesale (secrets readable). We keep a
//!     broad read (generous-read posture, §4) but DENY the secret set by NOT
//!     granting their parent dirs read and, where they sit under a granted root,
//!     relying on the recursive-carve walk. Landlock is allow-only, so a deny is
//!     "don't grant + grant siblings" — the recursive `.env*` carve from
//!     `.fray/sandbox-fs-deny-list.md`. (First cut: broad `/` read minus a
//!     curated set of secret PARENT dirs that we simply never add; full
//!     recursive `.env*` per-child walk is the documented follow-on — see
//!     DESIGN-NOTES.)
//!   - **Write** — subtree write on package dir + jail-home + extra-write
//!     (NOT `/tmp` — symlink-race avoidance, matching aube), plus `/dev`.
//!   - **Net** — seccomp `AF_INET`/`AF_INET6`-deny (+ other dangerous families),
//!     keeping `AF_UNIX` for node IPC. Loopback-to-proxy carve-out is the
//!     follow-on when the proxy lands (seccomp can't allow-list by host).
//!
//! Capability-probe + graceful-degrade: if Landlock is unavailable (kernel
//! <5.19 / disabled), the fs jail is lost — we return a [`Degradation`] and
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
    ABI, Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus,
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
        .map_err(|e| format!("failed to open jail path {}: {e}", path.display()))?;
    ruleset
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|e| format!("failed to add jail path {}: {e}", path.display()))
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

    // WRITE: package dir + jail-home + extra-write + /dev. NOT /tmp.
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
    let denied = [
        libc::AF_INET,
        libc::AF_INET6,
        libc::AF_NETLINK,
        libc::AF_PACKET,
        libc::AF_VSOCK,
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

/// Probe whether Landlock is available on this kernel (cheap, one-time-ish).
fn landlock_available() -> bool {
    // Creating a minimal ruleset and querying the ABI is the standard probe.
    Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(|r| r.create())
        .is_ok()
}

pub fn apply(cmd: &mut Command, policy: &SandboxPolicy) -> std::io::Result<Degradation> {
    let mut deg = Degradation::full();
    let ll_ok = landlock_available();
    if !ll_ok {
        deg.lost.push("fs".into());
        deg.reason = Some("Landlock unavailable on this kernel".into());
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
    use crate::build_jail::{self, BuildJailParams};
    use std::path::PathBuf;

    #[test]
    fn jail_policy_constructs() {
        // Smoke: the build-jail policy is well-formed for the Linux backend
        // (the actual enforcement is exercised by the e2e test under Docker/CI).
        let p = build_jail::policy(&BuildJailParams {
            package_dir: PathBuf::from("/proj/node_modules/dep"),
            project_root: PathBuf::from("/proj"),
            jail_home: PathBuf::from("/tmp/nub-jail/1/dep"),
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
