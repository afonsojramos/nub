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

use crate::policy::{Effect, FsAccess, FsPolicy, FsRule};
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
    /// A generous-read base (`default_effect == Allow`, OR a whole-fs `**` Allow entry
    /// — the shape the compiler actually emits for `"..."`/`sandbox: true`). The
    /// allowlist can't express read-all-minus-secrets; reads are confined to the
    /// explicit allow-set instead.
    generous_read: bool,
    /// An embedded-glob read allow — can't be a single inheritable ACE; skipped
    /// (fail-safe over-confinement rather than widening a grant to its literal prefix,
    /// which could expose a sibling secret).
    glob_read_unenforced: bool,
}

/// Derive the AppContainer read/write grants from the fs IR. Only LITERAL subtrees can
/// be expressed as an inheritable ACE; the read-confine (`default_effect == Deny`)
/// posture maps faithfully, while a generous-read base or an embedded-glob allow can't
/// and is reported via [`FsDegrade`] (fail-safe: over-confine + name it, never widen).
/// (The deny-shadowing check is done by [`deny_shadows_grant`] in `apply`, AFTER the
/// program-dir grant is folded into the read set.)
fn derive_grants(fs: &FsPolicy) -> (Vec<PathBuf>, Vec<PathBuf>, FsDegrade) {
    let mut read = Vec::new();
    let mut write = Vec::new();
    let mut degrade = FsDegrade {
        generous_read: fs.rules.default_effect == Effect::Allow,
        ..Default::default()
    };

    for rule in &fs.rules.entries {
        // Denies are implicit in the allowlist (ungranted = denied); their one hole (a
        // deny inside a granted subtree) is checked in `apply` post-program-dir.
        if rule.effect == Effect::Deny {
            continue;
        }
        match literal_subtree(rule.matcher.as_str()) {
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
            // A whole-fs `**` Allow is the generous-read base (what the compiler emits
            // for `"..."`/`sandbox: true` alongside a Deny base) — the allowlist can't
            // express it, so degrade and confine to the explicit allow-set. A NON-whole-
            // fs embedded glob is a distinct over-confinement (skipped, not widened).
            None if is_whole_fs(rule.matcher.as_str()) => degrade.generous_read = true,
            None if has_glob_meta(rule.matcher.as_str()) => degrade.glob_read_unenforced = true,
            None => {}
        }
    }
    (read, write, degrade)
}

/// Whether any read DENY could match a path inside a granted read subtree — an
/// inheritable read-allow on the grant DEFEATS such a deny on Windows (the same class
/// of trap the AAP denylist hits), so it cannot be carved and must be reported. The
/// rule is sound and conservative: a depth-independent glob deny (`**/.env`) shadows
/// EVERY grant, and a deny whose literal prefix is inside a grant (or vice-versa)
/// shadows it. Matching is case-insensitive (Windows paths are). Run with the FULL read
/// set (incl. the program-dir grant), since a deny landing under it is defeated too.
fn deny_shadows_grant(entries: &[FsRule], read_grants: &[PathBuf]) -> bool {
    if read_grants.is_empty() {
        return false;
    }
    for rule in entries {
        if rule.effect != Effect::Deny {
            continue;
        }
        let g = rule.matcher.as_str();
        // A depth-independent glob deny (no literal prefix before the first `**`, e.g.
        // `**/.env`) can match inside any granted subtree.
        let prefix = literal_prefix(g);
        if prefix.is_empty() {
            return true;
        }
        let dp = PathBuf::from(prefix);
        if read_grants
            .iter()
            .any(|grant| path_prefixes(grant, &dp) || path_prefixes(&dp, grant))
        {
            return true;
        }
    }
    false
}

/// The literal directory prefix of a glob — the leading run of full, glob-free path
/// components (e.g. `C:/proj/*.pem` → `C:/proj`, `**/.env` → ``, `C:/x` → `C:/x`).
fn literal_prefix(glob: &str) -> String {
    if !has_glob_meta(glob) {
        return glob.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    for comp in glob.split('/') {
        if has_glob_meta(comp) {
            break;
        }
        kept.push(comp);
    }
    kept.join("/")
}

/// Whether `a` is a path-prefix of `b` (component-wise, case-insensitive).
fn path_prefixes(a: &Path, b: &Path) -> bool {
    let mut bc = b.components();
    for ac in a.components() {
        match bc.next() {
            Some(bcomp) => {
                if !ac.as_os_str().eq_ignore_ascii_case(bcomp.as_os_str()) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
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

/// The ancestor directories that must be made AC-traversable so the LowBox child can
/// REACH each granted leaf. A LowBox token does NOT bypass traverse checking (proven
/// by the CI probe: a work dir under `%TEMP%` is unreachable because the
/// `C:\Users\<user>` chain grants traverse only to `Users`, which an AppContainer SID
/// is not in), so a leaf grant alone is insufficient — every ancestor up to the drive
/// root needs a traverse (execute-only) grant. The DRIVE ROOT itself is excluded: `C:\`
/// is AC-traversable by default (same probe), and mutating the drive-root ACL is both
/// unnecessary and heavy-handed. Ancestors that are themselves a granted leaf are
/// skipped (their stronger read/modify grant already admits traverse). Deduplicated.
fn ancestor_traverse_dirs(read: &[PathBuf], write: &[PathBuf]) -> Vec<PathBuf> {
    let leaves: Vec<&PathBuf> = read.iter().chain(write.iter()).collect();
    let mut out: Vec<PathBuf> = Vec::new();
    for leaf in &leaves {
        // `ancestors()` yields the path itself first; skip it. STOP at the drive root
        // (`C:\`) — never touch it. Drive-root detection is string-based (not
        // `Path::parent`), so the logic is identical on the Windows target and the host
        // where it's unit-tested (the host would not treat `C:` as a drive root).
        for anc in leaf.ancestors().skip(1) {
            if is_drive_root(anc) {
                break;
            }
            let anc = anc.to_path_buf();
            // A leaf already carries a read/modify grant (which includes traverse).
            if leaves.iter().any(|l| **l == anc) {
                continue;
            }
            if !out.contains(&anc) {
                out.push(anc);
            }
        }
    }
    out
}

/// Whether a path is a Windows drive root (`C:`, `C:\`, `C:/`) or an empty root remnant.
/// String-based so it's OS-independent (the host's `Path` semantics don't recognize a
/// drive letter, which would otherwise make the ancestor walk host-dependent).
fn is_drive_root(p: &Path) -> bool {
    let Some(s) = p.to_str() else { return false };
    let s = s.trim_end_matches(['/', '\\']);
    if s.is_empty() {
        return true;
    }
    let b = s.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

// ── the apply() entry (Windows-only: constructs Prepared.launch) ────────────────

#[cfg(target_os = "windows")]
pub(crate) fn apply(
    policy: &SandboxPolicy,
    spec: super::CommandSpec,
    proxy_port: Option<u16>,
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
        if let Some(port) = proxy_port {
            super::set_proxy_env(&mut command, port);
        }
        return Ok(Prepared {
            command,
            degradation: Degradation::full(),
            proxy: None,
            launch: None,
        });
    }

    let (read_grants, write_grants, fs_degrade) = derive_grants(&policy.fs);

    // Auto-grant read on the program's own directory so the LowBox child can exec +
    // load its sibling DLLs (system DLLs live under dirs that already grant ALL
    // APPLICATION PACKAGES, so those need no grant). Unlike macOS (file-only), a Windows
    // binary commonly loads sibling DLLs, so the DIR is granted. KNOWN EXPOSURE: this
    // read-grant is subtree-inheritable, so a project-local program's siblings (a `.env`
    // next to a tool) become readable — bounded for the build-jail (the program is the
    // toolchain, e.g. node.exe, whose dir holds no user secrets), but for a general
    // confined run the FRONT-END should own the program grant explicitly rather than the
    // engine auto-widening it. Documented follow-up, not silently claimed as full.
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
    // A read deny landing inside a granted subtree (incl. the program-dir grant) can't
    // be carved on Windows — the inheritable read-allow defeats it. Checked with the
    // FULL read set (after the program dir is folded in).
    if deny_shadows_grant(&policy.fs.rules.entries, &read_grants) {
        deg.lost.push("fs-read-deny".to_string());
        reason.get_or_insert_with(|| {
            "a read deny landing inside a granted subtree can't be carved on Windows \
             (inheritable allow wins) — deny not enforced"
                .to_string()
        });
    }
    // Coarse net: an enforced net with any Allow rule needs the loopback proxy for
    // per-host. On Windows the proxy runs in the parent, but an AppContainer child
    // cannot reach a loopback service without a registered loopback exemption
    // (`NetworkIsolationSetAppContainerConfig`) — NOT wired in this phase — so per-host
    // is honestly degraded and the coarse egress-deny (no `internetClient`) holds. The
    // branch-scoped windows-latest CI probe investigates the exemption's feasibility.
    if policy.net.enforce && policy.net.rules.iter().any(|r| r.effect == Effect::Allow) {
        deg.lost.push("net-per-host".to_string());
        reason.get_or_insert_with(|| {
            "per-host egress needs an AppContainer loopback exemption to reach the proxy \
             (not wired) — per-host allows denied (coarse network deny)"
                .to_string()
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
        // When env is enforced, fold the cooperative proxy hint into the constructed
        // map (over an enforced env-scrub). If env is NOT enforced, the child inherits
        // the ambient env and we do not synthesize a block just for the proxy vars —
        // per-host is degraded on Windows anyway, so the hint would be inert.
        env: policy.env.enforce.then(|| {
            let mut m = policy.env.constructed.clone();
            if let Some(port) = proxy_port {
                let url = format!("http://127.0.0.1:{port}");
                for k in [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "ALL_PROXY",
                ] {
                    m.insert(k.to_string(), url.clone());
                }
            }
            m
        }),
        // Grant internetClient only when net is unconfined; an enforced net is coarse
        // deny (no capability). Per-host would need the loopback exemption (above).
        allow_internet: !policy.net.enforce,
    };

    // The `command` field is unused on the launch path (status() runs `launch`); it
    // holds a benign never-spawned placeholder so the struct stays uniform.
    Ok(Prepared {
        command: std::process::Command::new(&launch.program),
        degradation: deg,
        proxy: None,
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
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
        SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
        NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
    };
    use windows_sys::Win32::Security::{
        ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, FreeSid, GetLengthSid,
        OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    // Generic access rights (avoid a Storage_FileSystem feature dep for FILE_GENERIC_*).
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const DELETE: u32 = 0x0001_0000;
    // Directory-specific rights for an ancestor traverse-only grant: FILE_TRAVERSE (the
    // "execute" right that lets a token pass THROUGH a dir) + FILE_READ_ATTRIBUTES (stat
    // it en route). No FILE_LIST_DIRECTORY ⇒ the child can pass through but not enumerate.
    const FILE_TRAVERSE: u32 = 0x0020;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    // ACE_FLAGS: applies to this object only (no inheritance).
    const NO_INHERITANCE: u32 = 0x0;
    // SE_GROUP_ENABLED — a capability SID in SECURITY_CAPABILITIES must be enabled.
    const SE_GROUP_ENABLED: u32 = 0x4;
    // The well-known internetClient capability SID.
    const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

    /// Monotonic per-process counter so concurrent launches never collide on the
    /// AppContainer profile name (combined with pid + a time nonce).
    static LAUNCH_CTR: AtomicU64 = AtomicU64::new(0);

    /// Serializes the per-path DACL read-modify-write in [`set_ace`]. Concurrent launches
    /// can grant/revoke traverse on a SHARED ancestor (e.g. `C:\Users\<me>`); without
    /// this, two non-atomic RMWs race and one run's ACE is lost (its leaf then
    /// unreachable). A single global lock is ample — ACL edits are brief and rare.
    static ACL_LOCK: Mutex<()> = Mutex::new(());

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

            // 2. Grant the allow-ACEs; `_aces` revokes them on drop (declared before
            //    the job ⇒ revoked after the tree is reaped, before profile delete).
            //    Leaf read/write grants are INHERITABLE (cover the subtree); ancestor
            //    grants are traverse-only + NON-inheritable (pass-through, no listing of
            //    the ancestor's other children). A REVOKE_ACCESS teardown on the unique
            //    SID removes exactly our ACEs from every path, whatever the access mask.
            let mut granted: Vec<std::path::PathBuf> = Vec::new();
            for dir in super::ancestor_traverse_dirs(&self.read_grants, &self.write_grants) {
                if set_ace(
                    &dir,
                    ac_sid,
                    FILE_TRAVERSE | FILE_READ_ATTRIBUTES,
                    GRANT_ACCESS,
                    false,
                )
                .is_ok()
                {
                    granted.push(dir);
                }
            }
            for dir in &self.read_grants {
                if set_ace(
                    dir,
                    ac_sid,
                    GENERIC_READ | GENERIC_EXECUTE,
                    GRANT_ACCESS,
                    true,
                )
                .is_ok()
                {
                    granted.push(dir.clone());
                }
            }
            for dir in &self.write_grants {
                let _ = set_ace(
                    dir,
                    ac_sid,
                    GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE | DELETE,
                    GRANT_ACCESS,
                    true,
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

            // 5. Proc-thread attribute list: SECURITY_CAPABILITIES, plus a HANDLE_LIST
            //    scoping inheritance to EXACTLY the std handles (see `bInheritHandles`
            //    below). The list must be alive across CreateProcessW (it stores the
            //    pointer); `inherit_handles` outlives the call.
            let inherit_handles = inheritable_std_handles();
            let n_attrs = 1 + u32::from(!inherit_handles.is_empty());
            let mut attr = ProcThreadAttrList::new(n_attrs)?;
            attr.update(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                std::ptr::from_mut(&mut sec_caps).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
            )?;
            if !inherit_handles.is_empty() {
                attr.update(
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    inherit_handles.as_ptr().cast_mut().cast(),
                    std::mem::size_of::<HANDLE>() * inherit_handles.len(),
                )?;
            }

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
                    // bInheritHandles must be TRUE for the PROC_THREAD_ATTRIBUTE_HANDLE_LIST
                    // above to take effect — and WITH that list, the child inherits ONLY the
                    // std handles in it (its output still reaches the user), not every
                    // inheritable handle nub holds. If there was no valid std handle to pass,
                    // the list is absent and we set FALSE (inherit nothing) — fail-safe.
                    i32::from(!inherit_handles.is_empty()),
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

            // 7. Assign to the job while the child is still SUSPENDED, and only resume
            //    once it is contained — so a child that spawns a descendant can never do
            //    so outside the Job. On assign failure, terminate the still-suspended
            //    child (it never ran) and fail closed.
            let assign_ok = unsafe { AssignProcessToJobObject(job, pi.hProcess) };
            if assign_ok == 0 {
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
                    CloseHandle(pi.hThread);
                    CloseHandle(pi.hProcess);
                }
                return Err(io::Error::other("AssignProcessToJobObject failed"));
            }
            unsafe { ResumeThread(pi.hThread) };

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
            // DeleteAppContainerProfile removes the profile (registry/on-disk state) but
            // does NOT free the SID buffer; per MSDN the SID from
            // CreateAppContainerProfile must be released with FreeSid. Independent calls,
            // no double-free.
            unsafe {
                DeleteAppContainerProfile(self.name.as_ptr());
                FreeSid(self.sid);
            }
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

    /// An initialized PROC_THREAD_ATTRIBUTE_LIST, backed by a pointer-aligned buffer
    /// (a `Vec<usize>`, not `Vec<u8>`, so the opaque list is suitably aligned), freed on
    /// drop.
    struct ProcThreadAttrList {
        buf: Vec<usize>,
    }
    impl ProcThreadAttrList {
        fn new(count: u32) -> io::Result<Self> {
            let mut size: usize = 0;
            // First call sizes the list (expected to "fail" setting size).
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
            let words = size.div_ceil(std::mem::size_of::<usize>()).max(1);
            let mut buf = vec![0usize; words];
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

    /// The std handles (stdin/stdout/stderr) to hand the child, deduplicated. Each is
    /// marked inheritable — every member of a PROC_THREAD_ATTRIBUTE_HANDLE_LIST must be,
    /// or CreateProcessW fails. An invalid/NULL std handle (a parent with no console) is
    /// skipped; an empty result ⇒ the caller inherits nothing (bInheritHandles FALSE).
    /// Marking the parent's own std handles inheritable is what `std`'s own inherited-stdio
    /// spawn does; it does not widen anything the child can reach beyond its stdio.
    fn inheritable_std_handles() -> Vec<HANDLE> {
        let raws = [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ];
        let mut out: Vec<HANDLE> = Vec::new();
        for r in raws {
            let h: HANDLE = r.cast();
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                continue;
            }
            // Only keep a handle we could actually mark inheritable — a non-inheritable
            // member would make CreateProcessW fail the whole spawn, so omit it (the child
            // loses that one stream) rather than take the process down.
            let marked =
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            if marked != 0 && !out.contains(&h) {
                out.push(h);
            }
        }
        out
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
        let len = unsafe { GetLengthSid(sid) } as usize;
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

    /// Remove every ACE for `sid` on `path` (teardown). REVOKE_ACCESS ignores the
    /// access mask + inheritance and matches purely on the trustee, so a unique per-run
    /// SID's ACEs go cleanly wherever we placed them.
    fn revoke_ace(path: &Path, sid: PSID) -> io::Result<()> {
        set_ace(path, sid, 0, REVOKE_ACCESS, false)
    }

    /// Add/remove an ACE granting `sid` `access` on `path`. `inherit` ⇒ the ACE is
    /// container+object inheritable (a leaf subtree grant); otherwise it applies to
    /// `path` alone (an ancestor traverse grant). Additive — reads the existing DACL and
    /// merges, never clobbering other ACEs.
    fn set_ace(path: &Path, sid: PSID, access: u32, mode: i32, inherit: bool) -> io::Result<()> {
        // Serialize the DACL RMW across concurrent launches (see ACL_LOCK). Poison-
        // tolerant: a prior panicked holder left no invariant broken here.
        let _lock: MutexGuard<'_, ()> = ACL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        ea.grfInheritance = if inherit {
            CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
        } else {
            NO_INHERITANCE
        };
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
    /// Entries are ordered case-INSENSITIVELY by key — the block ordering Windows
    /// expects (the source `BTreeMap` is case-sensitive, so a lowercase key like
    /// `windir` would otherwise sort after all-uppercase keys and violate the
    /// convention).
    fn build_env_block(env: &std::collections::BTreeMap<String, String>) -> Vec<u16> {
        let mut pairs: Vec<(&String, &String)> = env.iter().collect();
        pairs.sort_by_key(|a| a.0.to_ascii_uppercase());
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in pairs {
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
    fn whole_fs_allow_entry_degrades_generous_read() {
        // The shape the compiler ACTUALLY emits for `"..."` / `sandbox: true`: a Deny
        // base + a whole-fs `**` Allow ENTRY (+ secret denies). It must degrade, not be
        // silently dropped as a no-op grant.
        let p = fs(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("**/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let (read, _write, deg) = derive_grants(&p);
        assert!(
            read.is_empty(),
            "a whole-fs `**` allow yields no literal grant"
        );
        assert!(
            deg.generous_read,
            "a whole-fs `**` Allow ENTRY must degrade fs-read (not silently drop)"
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
    fn deny_shadowed_by_a_grant_is_detected() {
        let grants = vec![PathBuf::from("C:/proj")];
        // A LITERAL deny inside a granted subtree — inheritable allow defeats it.
        let literal = vec![rule("C:/proj/secret", Effect::Deny, FsAccess::Read)];
        assert!(deny_shadows_grant(&literal, &grants));
        // A GLOBBED deny inside the grant (`C:/proj/*.pem`) — the earlier gap: its
        // literal prefix `C:/proj` is the grant, so it's shadowed.
        let globbed = vec![rule("C:/proj/*.pem", Effect::Deny, FsAccess::Read)];
        assert!(deny_shadows_grant(&globbed, &grants));
        // A DEPTH-INDEPENDENT deny (`**/.env`) matches inside every grant.
        let depth_indep = vec![rule("**/.env", Effect::Deny, FsAccess::Read)];
        assert!(deny_shadows_grant(&depth_indep, &grants));
        // Case-insensitive: `C:/PROJ/...` still shadows the `C:/proj` grant.
        let cased = vec![rule("C:/PROJ/secret", Effect::Deny, FsAccess::Read)];
        assert!(deny_shadows_grant(&cased, &grants));
        // A deny OUTSIDE every grant is enforced by default-deny — not shadowed.
        let outside = vec![rule("C:/other/secret", Effect::Deny, FsAccess::Read)];
        assert!(!deny_shadows_grant(&outside, &grants));
        // No grants ⇒ nothing to shadow.
        assert!(!deny_shadows_grant(&depth_indep, &[]));
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
    fn ancestor_traverse_excludes_leaves_and_drive_root() {
        // A leaf nested under a user profile: every ancestor up to (not incl.) the drive
        // root is traverse-granted so the LowBox child can reach the leaf; `C:/` is not.
        let read = vec![PathBuf::from("C:/Users/me/proj/pkg")];
        let anc = ancestor_traverse_dirs(&read, &[]);
        assert_eq!(
            anc,
            vec![
                PathBuf::from("C:/Users/me/proj"),
                PathBuf::from("C:/Users/me"),
                PathBuf::from("C:/Users"),
            ],
            "ancestors up to but excluding the drive root"
        );
        // A leaf that is itself an ancestor of another leaf is not double-granted.
        let read = vec![PathBuf::from("C:/root/bin"), PathBuf::from("C:/root/work")];
        let anc = ancestor_traverse_dirs(&read, &[]);
        assert_eq!(anc, vec![PathBuf::from("C:/root")]);
        // A drive-root-direct leaf needs no ancestor grant (C:/ is AC-traversable).
        assert!(ancestor_traverse_dirs(&[PathBuf::from("C:/work")], &[]).is_empty());
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
