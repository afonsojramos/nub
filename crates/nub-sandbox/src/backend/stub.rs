//! Windows / other-OS scaffold backend.
//!
//! TODO(build-jail Windows Tier 1, `.fray/build-jail-design.md` §1 "Windows"):
//! the unprivileged OS write-jail is `CreateRestrictedToken(WRITE_RESTRICTED,
//! restrictingSids=[per-jail synthetic write-SID])` + `SetNamedSecurityInfo`
//! ACL grants on the writable roots, spawned `CREATE_SUSPENDED` → assign-to-job
//! → apply-token → `ResumeThread`. Plus the per-`.env*` deny-read ACEs and the
//! cap-SID inheritable allow-rw from `.fray/sandbox-fs-deny-list.md` (Windows
//! mechanism, MS-docs-confirmed, no admin). The Job-Object active-process /
//! memory limits (Tier 0) are already applied by aube's `windows_job.rs`
//! reaping path — the env-scrub (Tier 0) is applied by the engine's
//! `apply_env_scrub` regardless of OS.
//!
//! Until that lands, this backend enforces NOTHING at the OS layer (env-scrub
//! still applies via the caller) and reports the gap honestly so the caller
//! surfaces the reduced-mode WARNING. It is FAIL-SAFE in the sense that it never
//! claims enforcement it didn't deliver — but it is NOT yet at parity, which is
//! the explicit first-cut scope (the other OS backends are scaffolded/stubbed).

use crate::backend::Degradation;
use crate::policy::SandboxPolicy;
use std::process::Command;

pub fn apply(_cmd: &mut Command, policy: &SandboxPolicy) -> std::io::Result<Degradation> {
    let mut lost = Vec::new();
    if policy.fs.write_enforce || policy.fs.read_enforce {
        lost.push("fs".into());
    }
    if policy.net.enforce {
        lost.push("net".into());
    }
    Ok(Degradation {
        lost,
        reason: Some(
            "OS write/net jail not yet implemented on this platform (env-scrub only)".into(),
        ),
    })
}
