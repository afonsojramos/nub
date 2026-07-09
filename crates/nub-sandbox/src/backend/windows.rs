//! Windows backend: launch the child into an AppContainer (LowBox token) via a
//! custom `CreateProcessW` + `STARTUPINFOEX`/`SECURITY_CAPABILITIES`, confined by
//! the ALLOWLIST / default-deny model. CI-validated design (probe run 28276213658,
//! `tests/sandbox-win-probes/`); see design.md §2.4 and .fray/sandbox.md.
//!
//! THE ALLOWLIST MODEL (why NOT a deny-ACE denylist): a LowBox token can reach an
//! object ONLY where the object's ACL grants its AppContainer SID, a capability SID,
//! or `ALL APPLICATION PACKAGES`. Everything else is denied BY DEFAULT. So read-
//! confine = grant the AppContainer SID read-execute on ONLY the allowed dirs; every
//! other path fails closed with no per-file deny-ACE. The deny-ACE denylist is
//! ABANDONED — it is defeated whenever a secret sits under a dir carrying an
//! inherited `ALL APPLICATION PACKAGES` read grant (the AAP grant satisfies the
//! lowbox check before the file deny is reached). We grant a UNIQUE per-run
//! AppContainer SID and never grant AAP, so no inherited AAP can widen the allow-set.
//!
//! AXES:
//!   - fs read-confine: inheritable allow-ACE (AC SID, read+execute) on each allowed
//!     read subtree. Only the *default-deny* (read-confine) posture is expressible;
//!     a generous-read (`default_effect == Allow`) policy degrades — the allowlist
//!     cannot say "read everything except secrets" (see [`derive_grants`]).
//!   - fs write-confine: inheritable allow-ACE (AC SID, modify) on each write subtree.
//!   - env-scrub: the child env IS the policy's constructed map (`lpEnvironment`),
//!     built by construction exactly as the mac/linux backends do.
//!   - coarse egress: no `internetClient` capability ⇒ ALL egress (incl. loopback)
//!     is blocked; the capability is granted only when net is unconfined. Per-host is
//!     the egress proxy's job (S6) — reported degraded until then.
//!   - process-reap: a Job Object with `KILL_ON_JOB_CLOSE`; the whole tree dies when
//!     the job handle closes (after the child exits, or if nub does).
//!
//! ENV-READ ISOLATION FROM ASCENDANTS IS REDUCED (design.md §2.4): a same-user
//! `OpenProcess(PROCESS_VM_READ)` on the parent can read nub's environ; AppContainer
//! cannot block it. env-scrub of the child's OWN env holds; ascendant-env isolation
//! is v1-degraded-with-note (the dedicated-account backend is the post-v0 fix). We
//! report it, never silently claim it closed.
//!
//! THE LAUNCH SEAM: unlike mac/linux, this backend cannot hand the caller a pre-built
//! `std::process::Command` — the AppContainer launch needs a custom CreateProcess, a
//! Job assigned at creation, and per-run ACL grants TORN DOWN after the child exits.
//! So [`apply`] returns a [`WindowsLaunch`] plan on [`Prepared::launch`], and
//! `Prepared::status()` calls [`WindowsLaunch::run`], which owns setup → spawn → wait
//! → RAII teardown.

use crate::policy::{Effect, FsAccess, FsPolicy};
// Referenced only by the Windows-gated `apply`; the host build (module-under-test)
// never names it.
#[cfg(target_os = "windows")]
use crate::policy::SandboxPolicy;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A resolved AppContainer launch plan. All fields are OS-agnostic plain data so the
/// IR→plan derivation is unit-tested on the dev host; [`WindowsLaunch::run`] (the FFI)
/// is `#[cfg(windows)]`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) struct WindowsLaunch {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    /// Subtrees the AppContainer SID is granted inheritable read-execute.
    read_grants: Vec<PathBuf>,
    /// Subtrees the AppContainer SID is granted inheritable modify (read+write).
    write_grants: Vec<PathBuf>,
    /// `Some` ⇒ enforce env by construction (the child env IS this map). `None` ⇒
    /// inherit the ambient env untouched.
    env: Option<BTreeMap<String, String>>,
    /// Grant the `internetClient` capability (egress allowed). `false` ⇒ coarse deny.
    allow_internet: bool,
}

/// What the allowlist model could NOT express for a policy, so the caller can be told.
#[derive(Debug, Default, PartialEq)]
struct FsDegrade {
    /// A generous-read base (`default_effect == Allow`) — allowlist can't express
    /// read-all-minus-secrets; reads are confined to the explicit allow-set instead.
    generous_read: bool,
    /// An embedded-glob read allow — can't be a single inheritable ACE; skipped
    /// (fail-safe over-confinement rather than widening a grant to its literal prefix,
    /// which could expose a sibling secret).
    glob_read_unenforced: bool,
    /// A read DENY that lands inside a granted read subtree — an inheritable allow
    /// defeats it (the same class of trap the AAP denylist hits), so the deny is not
    /// honored. Never happens for a pure own-dir build-jail grant.
    deny_inside_grant: bool,
}

/// Derive the AppContainer read/write grants from the fs IR. Only LITERAL subtrees can
/// be expressed as an inheritable ACE; the read-confine (`default_effect == Deny`)
/// posture maps faithfully, while a generous-read base or an embedded-glob allow can't
/// and is reported via [`FsDegrade`] (fail-safe: over-confine + name it, never widen).
fn derive_grants(fs: &FsPolicy) -> (Vec<PathBuf>, Vec<PathBuf>, FsDegrade) {
    let mut read = Vec::new();
    let mut write = Vec::new();
    let mut degrade = FsDegrade {
        generous_read: fs.rules.default_effect == Effect::Allow,
        ..Default::default()
    };

    for rule in &fs.rules.entries {
        match rule.effect {
            Effect::Allow => match literal_subtree(rule.matcher.as_str()) {
                Some(dir) => {
                    if !read.contains(&dir) {
                        read.push(dir.clone());
                    }
                    if rule.access == FsAccess::ReadWrite
                        && !is_dangerous_write_root(&dir)
                        && !write.contains(&dir)
                    {
                        write.push(dir);
                    }
                }
                // An embedded-glob or whole-fs allow has no safe literal subtree to
                // grant: skip it (over-confine) rather than widen to a prefix that
                // could expose a sibling secret. A whole-fs `**` allow is the generous
                // base, already flagged by `generous_read` — only a NON-whole-fs glob
                // is a distinct over-confinement to surface.
                None if has_glob_meta(rule.matcher.as_str())
                    && !is_whole_fs(rule.matcher.as_str()) =>
                {
                    degrade.glob_read_unenforced = true;
                }
                None => {}
            },
            // Denies are implicit in the allowlist (ungranted = denied). The one hole:
            // a deny INSIDE a subtree we grant read — the inheritable allow-ACE defeats
            // it. Detect + report; it cannot be carved on Windows.
            Effect::Deny => {
                if let Some(dpath) = literal_subtree(rule.matcher.as_str())
                    && read.iter().any(|g| dpath.starts_with(g))
                {
                    degrade.deny_inside_grant = true;
                }
            }
        }
    }
    (read, write, degrade)
}

/// Whether a canonical IR glob contains glob metacharacters.
fn has_glob_meta(glob: &str) -> bool {
    glob.contains(['*', '?', '[', ']', '{', '}'])
}

/// Whether a glob addresses the whole filesystem (the generous-read base spellings).
fn is_whole_fs(glob: &str) -> bool {
    matches!(glob, "**" | "/**" | "/")
}

/// The literal directory subtree a matcher grants, or `None` if it can't be expressed
/// as one inheritable ACE. A plain absolute literal, or a literal + trailing `/**`
/// subtree twin, yields that directory; anything with embedded globs (or the whole-fs
/// spellings) yields `None`. Mirrors the macOS backend's `to_match_term` subpath case.
fn literal_subtree(glob: &str) -> Option<PathBuf> {
    if is_whole_fs(glob) {
        return None;
    }
    if !has_glob_meta(glob) {
        // A canonical IR path is absolute + forward-slashed; accept a Windows drive
        // path (`C:/…`) or a UNC/rooted path.
        return Some(PathBuf::from(glob));
    }
    if let Some(prefix) = glob.strip_suffix("/**")
        && !has_glob_meta(prefix)
    {
        return Some(PathBuf::from(prefix));
    }
    None
}

/// Top-level roots a WRITE grant must never cover — a `..`-collapsed surface path can
/// resolve to a system root, and an inheritable modify ACE there would be a
/// filesystem-wide write hole. The Windows twin of the macOS `is_dangerous_write_root`
/// (reads are exempt; a generous read is a legitimate posture, and read is separately
/// allowlist-confined here anyway). Matches on the forward-slashed canonical form.
fn is_dangerous_write_root(dir: &Path) -> bool {
    let Some(s) = dir.to_str() else { return false };
    let s = s.trim_end_matches('/');
    // Drive root (`C:`), the Windows dir, and Program Files are the roots a stray `..`
    // could land on. Case-insensitive: Windows paths are case-insensitive.
    let low = s.to_ascii_lowercase();
    if low.is_empty() || low == "/" {
        return true;
    }
    // `C:` / `C:/` — a bare drive root (2 chars + optional slash).
    let bytes = low.as_bytes();
    if bytes.len() <= 3 && bytes.get(1) == Some(&b':') {
        return true;
    }
    matches!(
        low.as_str(),
        "c:/windows"
            | "c:/windows/system32"
            | "c:/program files"
            | "c:/program files (x86)"
            | "c:/programdata"
            | "c:/users"
    )
}

/// Whether the fs axis confines anything (mirrors the mac/linux `fs_confines`). A
/// relaxed axis (`default_effect == Allow` with no entries) is not a confinement.
fn fs_confines(fs: &FsPolicy) -> bool {
    fs.rules.default_effect != Effect::Allow || !fs.rules.entries.is_empty()
}

// ── the apply() entry (Windows-only: constructs Prepared.launch) ────────────────

#[cfg(target_os = "windows")]
pub(crate) fn apply(
    policy: &SandboxPolicy,
    spec: super::CommandSpec,
) -> Result<super::Prepared, super::Degradation> {
    use super::{Degradation, Prepared};

    let confine_fs = fs_confines(&policy.fs);
    let sandboxing = confine_fs || policy.net.enforce;

    // Nothing needs the AppContainer: only env-scrub (or nothing). Use the plain
    // command path — identical contract to the mac/linux relaxed case.
    if !sandboxing {
        let mut command = std::process::Command::new(&spec.program);
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
        return Ok(Prepared {
            command,
            degradation: Degradation::full(),
            launch: None,
        });
    }

    let (read_grants, write_grants, fs_degrade) = derive_grants(&policy.fs);

    // Auto-grant read on the program's own directory so the LowBox child can exec +
    // load its sibling DLLs (system DLLs live under dirs that already grant ALL
    // APPLICATION PACKAGES, so those need no grant). Unlike macOS (file-only, to hide
    // a project-local tool's siblings), a Windows binary commonly loads sibling DLLs,
    // so the program dir is granted; for the build-jail the program is the toolchain,
    // whose dir is not a secret store.
    let mut read_grants = read_grants;
    if let Some(prog) = resolve_program(&spec.program, spec.cwd.as_deref())
        && let Some(parent) = prog.parent()
    {
        let parent = parent.to_path_buf();
        if !read_grants.contains(&parent) {
            read_grants.push(parent);
        }
    }

    // ── degradation (fail-safe-not-silent) ──────────────────────────────────────
    let mut deg = Degradation::full();
    let mut reason: Option<String> = None;
    if fs_degrade.generous_read {
        deg.lost.push("fs-read".to_string());
        reason.get_or_insert_with(|| {
            "AppContainer enforces an allowlist — a generous read-all-minus-secrets \
             policy is not expressible; reads confined to the explicit allow-set"
                .to_string()
        });
    }
    if fs_degrade.glob_read_unenforced {
        deg.lost.push("fs-read-glob".to_string());
        reason.get_or_insert_with(|| {
            "an embedded-glob read allow can't be an inheritable ACE — that path is \
             not read-granted (over-confined)"
                .to_string()
        });
    }
    if fs_degrade.deny_inside_grant {
        deg.lost.push("fs-read-deny".to_string());
        reason.get_or_insert_with(|| {
            "a read deny landing inside a granted subtree can't be carved on Windows \
             (inheritable allow wins) — deny not enforced"
                .to_string()
        });
    }
    // Coarse net: an enforced net with any Allow rule needs the proxy (S6) for per-host.
    if policy.net.enforce && policy.net.rules.iter().any(|r| r.effect == Effect::Allow) {
        deg.lost.push("net-per-host".to_string());
        reason.get_or_insert_with(|| {
            "egress proxy not wired — per-host allows denied (coarse network deny)".to_string()
        });
    }
    // Env-read isolation from ascendants is REDUCED on Windows (same-user
    // PROCESS_VM_READ) whenever the scrub actually withholds something. env-scrub of
    // the child's own env still holds; this names the residual, per design.md §2.4.
    if policy.env.enforce && !policy.env.withheld.is_empty() {
        deg.lost.push("env-read-ascendant".to_string());
        reason.get_or_insert_with(|| {
            "same-user PROCESS_VM_READ can read the parent's env — ascendant-env \
             isolation is reduced on Windows (dedicated-account backend is the fix)"
                .to_string()
        });
    }
    deg.reason = reason;

    let launch = WindowsLaunch {
        program: spec.program,
        args: spec.args,
        cwd: spec.cwd,
        read_grants,
        write_grants,
        env: policy.env.enforce.then(|| policy.env.constructed.clone()),
        // Grant internetClient only when net is unconfined; an enforced net is coarse
        // deny (no capability), per-host handled by the proxy later.
        allow_internet: !policy.net.enforce,
    };

    // The `command` field is unused on the launch path (status() runs `launch`); it
    // holds a benign never-spawned placeholder so the struct stays uniform.
    Ok(Prepared {
        command: std::process::Command::new(&launch.program),
        degradation: deg,
        launch: Some(launch),
    })
}

/// Resolve a program to an absolute path (best-effort) so its parent dir can be
/// read-granted and so CreateProcess needn't PATH-search under the LowBox token.
/// Absolute → itself; a path with a separator → joined against the child cwd; a bare
/// name → PATH search trying the name and common executable extensions. Windows-only
/// (its PATHEXT search is Windows semantics; the host build never calls it).
#[cfg(target_os = "windows")]
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
    let has_ext = p.extension().is_some();
    let exts = ["exe", "cmd", "bat", "com"];
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if has_ext {
            let cand = dir.join(p);
            if cand.is_file() {
                return Some(cand);
            }
        } else {
            for ext in exts {
                let cand = dir.join(format!("{}.{ext}", program.to_string_lossy()));
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

// ── the FFI launcher ────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod launch {
    use super::WindowsLaunch;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree, WAIT_OBJECT_0};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
        NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
    };
    use windows_sys::Win32::Security::{
        ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };

    // Generic access rights (avoid a Storage_FileSystem feature dep for FILE_GENERIC_*).
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const DELETE: u32 = 0x0001_0000;
    // SE_GROUP_ENABLED — a capability SID in SECURITY_CAPABILITIES must be enabled.
    const SE_GROUP_ENABLED: u32 = 0x4;
    // The well-known internetClient capability SID.
    const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

    /// Monotonic per-process counter so concurrent launches never collide on the
    /// AppContainer profile name (combined with pid + a time nonce).
    static LAUNCH_CTR: AtomicU64 = AtomicU64::new(0);

    impl WindowsLaunch {
        /// Own the full spawn lifecycle: create a per-run AppContainer profile, grant
        /// the inheritable allow-ACEs, launch the child under the LowBox token inside a
        /// kill-on-close Job, wait, then tear everything down (RAII).
        pub(crate) fn run(self) -> io::Result<ExitStatus> {
            // 1. Per-run AppContainer profile → AC SID. `_profile` deletes it on drop
            //    (declared FIRST ⇒ dropped LAST, after the ACEs are revoked).
            let name = unique_profile_name();
            let ac_sid = create_appcontainer(&name)?;
            let _profile = ProfileGuard {
                name: to_wide(&name),
                sid: ac_sid,
            };
            // An owned copy of the SID bytes, so ACE revoke doesn't depend on the
            // profile-owned SID pointer surviving.
            let sid_copy = copy_sid(ac_sid)?;

            // 2. Grant inheritable allow-ACEs; `_aces` revokes them on drop (declared
            //    before the job ⇒ revoked after the tree is reaped, before profile del).
            let mut granted: Vec<std::path::PathBuf> = Vec::new();
            for dir in &self.read_grants {
                if grant_ace(dir, ac_sid, GENERIC_READ | GENERIC_EXECUTE).is_ok() {
                    granted.push(dir.clone());
                }
            }
            for dir in &self.write_grants {
                let _ = grant_ace(
                    dir,
                    ac_sid,
                    GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE | DELETE,
                );
                if !granted.contains(dir) {
                    granted.push(dir.clone());
                }
            }
            let _aces = AceGuard {
                paths: granted,
                sid: sid_copy,
            };

            // 3. Capabilities: internetClient iff egress allowed.
            let mut cap_sid_owned: Option<CapSid> = None;
            let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
            if self.allow_internet {
                let cs = CapSid::new(INTERNET_CLIENT_SID)?;
                caps.push(SID_AND_ATTRIBUTES {
                    Sid: cs.0,
                    Attributes: SE_GROUP_ENABLED,
                });
                cap_sid_owned = Some(cs);
            }
            let mut sec_caps = SECURITY_CAPABILITIES {
                AppContainerSid: ac_sid,
                Capabilities: if caps.is_empty() {
                    std::ptr::null_mut()
                } else {
                    caps.as_mut_ptr()
                },
                CapabilityCount: caps.len() as u32,
                Reserved: 0,
            };

            // 4. Job with KILL_ON_JOB_CLOSE; `_job` closes the handle on drop (declared
            //    LAST ⇒ dropped FIRST ⇒ reaps any lingering tree before ACE revoke).
            let job = create_kill_on_close_job()?;
            let _job = HandleGuard(job);

            // 5. Proc-thread attribute list carrying SECURITY_CAPABILITIES.
            let mut attr = ProcThreadAttrList::new(1)?;
            attr.update(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                std::ptr::from_mut(&mut sec_caps).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
            )?;

            // 6. Build the command line + env block + cwd (kept alive across the call).
            let mut cmdline = build_command_line(&self.program, &self.args);
            let env_block = self.env.as_ref().map(build_env_block);
            let cwd_wide = self.cwd.as_ref().map(|c| to_wide(&c.to_string_lossy()));

            let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = attr.as_ptr();
            let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

            let mut flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED;
            let env_ptr: *const std::ffi::c_void = match &env_block {
                Some(b) => {
                    flags |= CREATE_UNICODE_ENVIRONMENT;
                    b.as_ptr().cast()
                }
                None => std::ptr::null(),
            };
            let cwd_ptr = cwd_wide.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());

            // SAFETY: cmdline/env_block/cwd_wide/attr/sec_caps/caps all outlive this
            // call; lpCommandLine is a writable UTF-16 buffer as CreateProcessW requires.
            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    cmdline.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    // bInheritHandles TRUE ⇒ the child shares the console/stdio, so its
                    // output reaches the user (parity with Command::status()).
                    1,
                    flags,
                    env_ptr as *const _,
                    cwd_ptr,
                    std::ptr::from_mut(&mut si).cast(),
                    &mut pi,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let _ = cap_sid_owned; // held alive until here

            // 7. Assign to the job BEFORE resuming, so the child (and any descendant it
            //    spawns) is captured; then resume and wait.
            let assign_ok = unsafe { AssignProcessToJobObject(job, pi.hProcess) };
            unsafe { ResumeThread(pi.hThread) };
            if assign_ok == 0 {
                // Could not contain the tree — reap what we have and fail closed rather
                // than let an unreaped tree run.
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
                    CloseHandle(pi.hThread);
                    CloseHandle(pi.hProcess);
                }
                return Err(io::Error::other("AssignProcessToJobObject failed"));
            }

            let code = unsafe {
                if WaitForSingleObject(pi.hProcess, INFINITE) != WAIT_OBJECT_0 {
                    let e = io::Error::last_os_error();
                    CloseHandle(pi.hThread);
                    CloseHandle(pi.hProcess);
                    return Err(e);
                }
                let mut code: u32 = 0;
                GetExitCodeProcess(pi.hProcess, &mut code);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
                code
            };

            Ok(ExitStatus::from_raw(code))
            // `_job` (reap) → `_aces` (revoke) → `_profile` (delete) drop here, reverse.
        }
    }

    /// A capability SID string converted to a PSID (LocalFree'd on drop).
    struct CapSid(PSID);
    impl CapSid {
        fn new(sid_str: &str) -> io::Result<Self> {
            let wide = to_wide(sid_str);
            let mut sid: PSID = std::ptr::null_mut();
            let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(CapSid(sid))
        }
    }
    impl Drop for CapSid {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }

    /// Deletes the per-run AppContainer profile and frees the AC SID on drop.
    struct ProfileGuard {
        name: Vec<u16>,
        sid: PSID,
    }
    impl Drop for ProfileGuard {
        fn drop(&mut self) {
            // DeleteAppContainerProfile also frees the SID CreateAppContainerProfile
            // returned, so we do NOT FreeSid separately (double-free otherwise).
            unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
            let _ = self.sid;
        }
    }

    /// Revokes the per-run allow-ACEs on drop. Uses an owned SID copy so it does not
    /// depend on the profile SID pointer. REVOKE_ACCESS removes every ACE for the SID;
    /// since the SID is unique per run and appears nowhere else, exactly our ACE goes.
    struct AceGuard {
        paths: Vec<std::path::PathBuf>,
        sid: Vec<u8>,
    }
    impl Drop for AceGuard {
        fn drop(&mut self) {
            let sid = self.sid.as_ptr() as PSID;
            for p in &self.paths {
                let _ = revoke_ace(p, sid);
            }
        }
    }

    /// Closes a raw handle on drop. For the Job handle this triggers
    /// KILL_ON_JOB_CLOSE — reaping any process still in the tree.
    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// An initialized PROC_THREAD_ATTRIBUTE_LIST (heap Vec-backed) freed on drop.
    struct ProcThreadAttrList {
        buf: Vec<u8>,
    }
    impl ProcThreadAttrList {
        fn new(count: u32) -> io::Result<Self> {
            let mut size: usize = 0;
            // First call sizes the list (expected to "fail" setting size).
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
            let mut buf = vec![0u8; size];
            let ok = unsafe {
                InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &mut size)
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { buf })
        }
        fn update(
            &mut self,
            attr: usize,
            value: *mut std::ffi::c_void,
            size: usize,
        ) -> io::Result<()> {
            let ok = unsafe {
                UpdateProcThreadAttribute(
                    self.buf.as_mut_ptr().cast(),
                    0,
                    attr,
                    value,
                    size,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        fn as_ptr(&mut self) -> *mut std::ffi::c_void {
            self.buf.as_mut_ptr().cast()
        }
    }
    impl Drop for ProcThreadAttrList {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.buf.as_mut_ptr().cast()) };
        }
    }

    fn unique_profile_name() -> String {
        let pid = std::process::id();
        let ctr = LAUNCH_CTR.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // AppContainer names are <= 64 chars, alnum/underscore.
        format!("nub_sbx_{pid}_{nonce:x}_{ctr}")
    }

    fn create_appcontainer(name: &str) -> io::Result<PSID> {
        let wname = to_wide(name);
        let mut sid: PSID = std::ptr::null_mut();
        // hr is an HRESULT; 0 == S_OK. Display name + description reuse the name.
        let hr = unsafe {
            CreateAppContainerProfile(
                wname.as_ptr(),
                wname.as_ptr(),
                wname.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if hr != 0 {
            return Err(io::Error::other(format!(
                "CreateAppContainerProfile failed hr=0x{hr:08x}"
            )));
        }
        Ok(sid)
    }

    /// Copy a PSID's bytes into an owned buffer (GetLengthSid).
    fn copy_sid(sid: PSID) -> io::Result<Vec<u8>> {
        let len = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; len];
        unsafe { std::ptr::copy_nonoverlapping(sid.cast::<u8>(), buf.as_mut_ptr(), len) };
        Ok(buf)
    }

    fn create_kill_on_close_job() -> io::Result<HANDLE> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(job) };
            return Err(e);
        }
        Ok(job)
    }

    /// Add an inheritable (container+object) allow-ACE granting `sid` `access` on `path`.
    fn grant_ace(path: &Path, sid: PSID, access: u32) -> io::Result<()> {
        set_ace(path, sid, access, GRANT_ACCESS)
    }
    fn revoke_ace(path: &Path, sid: PSID) -> io::Result<()> {
        set_ace(path, sid, 0, REVOKE_ACCESS)
    }

    fn set_ace(path: &Path, sid: PSID, access: u32, mode: i32) -> io::Result<()> {
        let wpath = to_wide_path(path);
        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // Read the existing DACL so the grant is additive (never clobber existing ACEs).
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let sd_guard = LocalFreeGuard(sd);

        let mut ea: EXPLICIT_ACCESS_W = unsafe { std::mem::zeroed() };
        ea.grfAccessPermissions = access;
        ea.grfAccessMode = mode;
        ea.grfInheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
        ea.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        };

        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let new_guard = LocalFreeGuard(new_dacl.cast());

        let rc = unsafe {
            SetNamedSecurityInfoW(
                wpath.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null_mut(),
            )
        };
        drop(new_guard);
        drop(sd_guard);
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }

    struct LocalFreeGuard(*mut std::ffi::c_void);
    impl Drop for LocalFreeGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// UTF-16, NUL-terminated.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A path as a NUL-terminated wide string with backslash separators (canonical IR
    /// paths are forward-slashed; the Win32 security APIs want native separators).
    fn to_wide_path(p: &Path) -> Vec<u16> {
        let s = p.to_string_lossy().replace('/', "\\");
        to_wide(&s)
    }

    /// Build a mutable UTF-16 command line from program + args, quoting each token per
    /// the CommandLineToArgvW rules std uses. lpApplicationName is NULL, so the child
    /// gets a conventional argv.
    fn build_command_line(program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> Vec<u16> {
        let mut line: Vec<u16> = Vec::new();
        append_quoted(&mut line, program);
        for a in args {
            line.push(u16::from(b' '));
            append_quoted(&mut line, a);
        }
        line.push(0);
        line
    }

    fn append_quoted(out: &mut Vec<u16>, arg: &std::ffi::OsStr) {
        let wide: Vec<u16> = arg.encode_wide().collect();
        let needs_quote = wide.is_empty()
            || wide
                .iter()
                .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
        if !needs_quote {
            out.extend_from_slice(&wide);
            return;
        }
        out.push(u16::from(b'"'));
        let mut backslashes = 0usize;
        for &c in &wide {
            if c == u16::from(b'\\') {
                backslashes += 1;
            } else if c == u16::from(b'"') {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push(u16::from(b'\\'));
                }
                out.push(u16::from(b'"'));
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push(u16::from(b'\\'));
                }
                backslashes = 0;
                out.push(c);
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push(u16::from(b'\\'));
        }
        out.push(u16::from(b'"'));
    }

    /// Build a UTF-16 double-NUL-terminated environment block from the constructed map.
    fn build_env_block(env: &std::collections::BTreeMap<String, String>) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in env {
            block.extend(k.encode_utf16());
            block.push(u16::from(b'='));
            block.extend(v.encode_utf16());
            block.push(0);
        }
        // An empty block still needs the terminating double-NUL.
        block.push(0);
        if block.len() == 1 {
            block.push(0);
        }
        block
    }
}

// ── host-testable derivation tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{CanonGlob, FsRule, FsRuleSet, TmpMode};

    fn fs(default_effect: Effect, entries: Vec<FsRule>) -> FsPolicy {
        FsPolicy {
            rules: FsRuleSet {
                entries,
                default_effect,
            },
            tmp: TmpMode::Private,
        }
    }
    fn rule(m: &str, effect: Effect, access: FsAccess) -> FsRule {
        FsRule {
            matcher: CanonGlob(m.to_string()),
            effect,
            access,
        }
    }

    #[test]
    fn read_confine_grants_only_explicit_allows_no_degrade() {
        // default-deny + a literal own-dir rw allow = the build-jail shape: one read
        // grant + one write grant, no degradation.
        let p = fs(
            Effect::Deny,
            vec![rule("C:/proj/pkg", Effect::Allow, FsAccess::ReadWrite)],
        );
        let (read, write, deg) = derive_grants(&p);
        assert_eq!(read, vec![PathBuf::from("C:/proj/pkg")]);
        assert_eq!(write, vec![PathBuf::from("C:/proj/pkg")]);
        assert_eq!(deg, FsDegrade::default());
    }

    #[test]
    fn read_only_allow_yields_no_write_grant() {
        let p = fs(
            Effect::Deny,
            vec![rule("C:/tools", Effect::Allow, FsAccess::Read)],
        );
        let (read, write, _) = derive_grants(&p);
        assert_eq!(read, vec![PathBuf::from("C:/tools")]);
        assert!(
            write.is_empty(),
            "a read-only allow must not open a write grant"
        );
    }

    #[test]
    fn subtree_twin_collapses_to_the_directory() {
        // `C:/proj/**` and `C:/proj` both mean the subtree — one grant.
        assert_eq!(
            literal_subtree("C:/proj/**"),
            Some(PathBuf::from("C:/proj"))
        );
        assert_eq!(literal_subtree("C:/proj"), Some(PathBuf::from("C:/proj")));
    }

    #[test]
    fn generous_read_base_degrades_fs_read() {
        // default-allow (generous read-all-minus-secrets) can't be an allowlist.
        let p = fs(
            Effect::Allow,
            vec![rule("**/.env", Effect::Deny, FsAccess::Read)],
        );
        let (_read, _write, deg) = derive_grants(&p);
        assert!(
            deg.generous_read,
            "a default-Allow base must degrade fs-read"
        );
    }

    #[test]
    fn embedded_glob_allow_is_skipped_not_widened() {
        // `C:/proj/*.pem` must NOT widen to a `C:/proj` read grant (would expose a
        // sibling secret); it is skipped + flagged (fail-safe over-confinement).
        let p = fs(
            Effect::Deny,
            vec![rule("C:/proj/*.pem", Effect::Allow, FsAccess::Read)],
        );
        let (read, _write, deg) = derive_grants(&p);
        assert!(
            read.is_empty(),
            "an embedded-glob allow must not be widened to a grant"
        );
        assert!(deg.glob_read_unenforced);
    }

    #[test]
    fn deny_inside_a_granted_subtree_is_reported() {
        // Grant read on C:/proj, deny C:/proj/secret — the inheritable allow defeats
        // the deny on Windows, so it must be surfaced (not silently unenforced).
        let p = fs(
            Effect::Deny,
            vec![
                rule("C:/proj", Effect::Allow, FsAccess::ReadWrite),
                rule("C:/proj/secret", Effect::Deny, FsAccess::Read),
            ],
        );
        let (_read, _write, deg) = derive_grants(&p);
        assert!(
            deg.deny_inside_grant,
            "a deny inside a granted subtree must be reported"
        );
    }

    #[test]
    fn dangerous_write_roots_never_get_a_write_grant() {
        // A rw allow that resolves to a system root must not open an inheritable modify
        // ACE there (filesystem-wide write hole). Read of it is still fine.
        for root in ["C:", "C:/", "C:/Windows", "C:/Program Files", "C:/Users"] {
            let p = fs(
                Effect::Deny,
                vec![rule(root, Effect::Allow, FsAccess::ReadWrite)],
            );
            let (_read, write, _) = derive_grants(&p);
            assert!(
                write.is_empty(),
                "{root} must not receive a write grant (dangerous root)"
            );
        }
        // A real project dir under Users is NOT over-blocked.
        let p = fs(
            Effect::Deny,
            vec![rule("C:/Users/me/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let (_r, write, _) = derive_grants(&p);
        assert_eq!(write, vec![PathBuf::from("C:/Users/me/proj")]);
    }

    #[test]
    fn whole_fs_globs_have_no_literal_subtree() {
        assert_eq!(literal_subtree("**"), None);
        assert_eq!(literal_subtree("/**"), None);
        assert_eq!(literal_subtree("/"), None);
    }

    #[test]
    fn fs_confines_matches_mac_linux_semantics() {
        // Relaxed (default-Allow, no entries) does NOT confine.
        assert!(!fs_confines(&fs(Effect::Allow, vec![])));
        // Any entry, or a deny base, confines.
        assert!(fs_confines(&fs(Effect::Deny, vec![])));
        assert!(fs_confines(&fs(
            Effect::Allow,
            vec![rule("C:/x", Effect::Deny, FsAccess::Read)]
        )));
    }
}
