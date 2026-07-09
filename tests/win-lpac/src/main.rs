//! LPAC vs standard AppContainer — empirical windows-latest probe.
//!
//! ONE question: does an LPAC (Less Privileged AppContainer) launch EXCLUDE the ALL
//! APPLICATION PACKAGES SID (S-1-15-2-1) from the access check, so that (a) a file
//! reachable ONLY via an inherited AAP allow becomes unreadable, and (b) a per-file
//! explicit AC-SID deny inside a broad AC-SID allow is no longer defeated by that AAP
//! allow — i.e. deny-inside-a-broad-allow becomes EXPRESSIBLE. Plus: is LPAC VIABLE
//! (does a normal exe / node.exe start under it with a leaf grant)?
//!
//! The probe is BOTH runner and child (self-reexec `__child__ <role> [args]`), so no
//! separate compiled child and the role survives env handling. ONE AppContainer identity
//! is created and launched both standard and LPAC — the ONLY variable between the paired
//! cases is the AAP opt-out — so the differential is clean.
//!
//! Exit-code contract (child): 0 read-ok, 5 access-denied, 9 other-io-error,
//! 21 is-appcontainer, 20 not-appcontainer. The runner prints a full CASE matrix and
//! ALWAYS exits 0 on a completed matrix (a "confinement failed" line is DATA, not a
//! harness failure) — a nonzero runner exit means the harness itself broke.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("win-lpac-probe: windows-only; no-op on this host");
}

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__child__") {
        std::process::exit(win::child_main(&args[2..]));
    }
    win::run_matrix();
}

#[cfg(target_os = "windows")]
mod win {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree, WAIT_OBJECT_0};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT,
        SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        NO_MULTIPLE_TRUSTEE,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
    };
    use windows_sys::Win32::Security::{
        ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetTokenInformation, OBJECT_INHERIT_ACE,
        PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, TOKEN_QUERY, TokenIsAppContainer,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, OpenProcessToken,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    // ── constants ────────────────────────────────────────────────────────────────
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const DENY_ACCESS: i32 = 3; // SET_ACCESS=2, DENY_ACCESS=3 (ACCESS_MODE)
    const NO_INHERITANCE: u32 = 0x0;
    // SE_DACL_PROTECTED — set with SetNamedSecurityInfoW to drop inherited ACEs so the
    // tree carries EXACTLY the ACEs the probe places (no ambient %TEMP% AAP confound).
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    // PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY = ProcThreadAttributeValue(15,
    // FALSE, TRUE, FALSE) = 15 | PROC_THREAD_ATTRIBUTE_INPUT(0x20000) = 0x2000F. (windows-sys
    // 0.61 may not export it; define raw. Cross-checked against the exported
    // PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES = ProcThreadAttributeValue(9,..) = 0x20009.)
    const PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY: usize = 0x0002_000F;
    const PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 0x1;
    const AAP_SID: &str = "S-1-15-2-1"; // ALL APPLICATION PACKAGES
    const SE_GROUP_ENABLED: u32 = 0x4;
    const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

    // ── the child ────────────────────────────────────────────────────────────────
    pub fn child_main(a: &[String]) -> i32 {
        match a.first().map(String::as_str) {
            Some("read") => match std::fs::read(&a[1]) {
                Ok(_) => 0,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => 5,
                Err(_) => 9,
            },
            Some("token") => {
                let mut tok: HANDLE = std::ptr::null_mut();
                let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) };
                if ok == 0 {
                    return 20;
                }
                let mut is_ac: u32 = 0;
                let mut ret: u32 = 0;
                let ok = unsafe {
                    GetTokenInformation(
                        tok,
                        TokenIsAppContainer,
                        std::ptr::from_mut(&mut is_ac).cast(),
                        std::mem::size_of::<u32>() as u32,
                        &mut ret,
                    )
                };
                unsafe { CloseHandle(tok) };
                if ok != 0 && is_ac != 0 { 21 } else { 20 }
            }
            _ => 99,
        }
    }

    // ── the matrix ───────────────────────────────────────────────────────────────
    pub fn run_matrix() {
        println!("=== win-lpac-probe matrix ===");
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().unwrap().to_path_buf();

        // One AppContainer identity, reused for every launch.
        let name = format!("nub_lpac_probe_{}", std::process::id());
        let ac_sid = match create_appcontainer(&name) {
            Ok(s) => s,
            Err(e) => {
                println!("FATAL create_appcontainer: {e}");
                std::process::exit(2);
            }
        };
        // Grant the AC-SID read+execute on the probe exe dir so the LowBox/LPAC child can
        // load + run it (system DLLs carry ARAP → loadable under LPAC; crt-static avoids
        // vcruntime). Additive, inheritable.
        if let Err(e) = add_ace(&exe_dir, ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true) {
            println!("WARN grant exe_dir: {e}");
        }

        // Sanity: does the container even launch, and is the token an AppContainer?
        report("T0.std.token", launch(&exe, &["__child__", "token"], &exe_dir, ac_sid, false, false));
        report("T0.lpac.token", launch(&exe, &["__child__", "token"], &exe_dir, ac_sid, true, false));

        // ── ATOMIC A: pure AAP exclusion. A file reachable ONLY via inherited AAP allow
        //    (NO AC-SID grant). Standard AC reads it (AAP in token); LPAC must NOT.
        {
            let dir = mktree("A_aaponly");
            let f = dir.join("f.txt");
            std::fs::write(&f, b"secret").unwrap();
            // Root: protected, owner FC + AAP inheritable read. NO AC-SID.
            set_protected_dacl(&dir, &[
                (current_user_sid_ptr(), 0x1F01FF, GRANT_ACCESS, true), // owner full
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true),
            ]);
            report("A.std  (AAP-only file, standard AC — EXPECT 0 readable)",
                launch(&exe, &["__child__", "read", &f.to_string_lossy()], &exe_dir, ac_sid, false, false));
            report("A.lpac (AAP-only file, LPAC — EXPECT 5 DENIED if LPAC excludes AAP)",
                launch(&exe, &["__child__", "read", &f.to_string_lossy()], &exe_dir, ac_sid, true, false));
        }

        // ── ATOMIC B/C/D: deny-inside-a-broad-allow. Root: owner FC + AC-SID broad read
        //    allow (inheritable) + AAP inheritable read allow. pub.txt inherits both;
        //    secret.txt additionally carries an explicit AC-SID deny (B/D) or AC-SID+AAP
        //    deny (C).
        {
            let dir = mktree("BCD_denyinside");
            let pubf = dir.join("pub.txt");
            let secret = dir.join("secret.txt");
            std::fs::write(&pubf, b"public").unwrap();
            std::fs::write(&secret, b"SECRET").unwrap();
            set_protected_dacl(&dir, &[
                (current_user_sid_ptr(), 0x1F01FF, GRANT_ACCESS, true),
                (ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true),
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true),
            ]);
            // secret.txt: explicit AC-SID deny (canonical order puts explicit deny first).
            add_ace(&secret, ac_sid, GENERIC_READ | GENERIC_EXECUTE, DENY_ACCESS, false).ok();

            report("B.std.pub    (broad allow, standard AC — EXPECT 0)",
                launch(&exe, &["__child__", "read", &pubf.to_string_lossy()], &exe_dir, ac_sid, false, false));
            report("B.std.secret (AC-SID deny inside allow+AAP, standard AC — 5=carve works, 0=AAP defeats deny [TRAP])",
                launch(&exe, &["__child__", "read", &secret.to_string_lossy()], &exe_dir, ac_sid, false, false));
            report("D.lpac.pub    (broad allow, LPAC — EXPECT 0)",
                launch(&exe, &["__child__", "read", &pubf.to_string_lossy()], &exe_dir, ac_sid, true, false));
            report("D.lpac.secret (AC-SID deny inside allow+AAP, LPAC — EXPECT 5 if LPAC closes deny-inside-allow)",
                launch(&exe, &["__child__", "read", &secret.to_string_lossy()], &exe_dir, ac_sid, true, false));
        }

        // ── ATOMIC C: standard AC, deny BOTH AC-SID AND AAP on the secret — the non-LPAC
        //    alternative. If this denies, deny-inside-allow is expressible WITHOUT LPAC by
        //    also emitting an AAP deny-ACE.
        {
            let dir = mktree("C_denyboth");
            let secret = dir.join("secret.txt");
            std::fs::write(&secret, b"SECRET").unwrap();
            set_protected_dacl(&dir, &[
                (current_user_sid_ptr(), 0x1F01FF, GRANT_ACCESS, true),
                (ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true),
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true),
            ]);
            add_ace(&secret, ac_sid, GENERIC_READ | GENERIC_EXECUTE, DENY_ACCESS, false).ok();
            add_ace(&secret, sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, DENY_ACCESS, false).ok();
            report("C.std.secret (deny AC-SID+AAP, standard AC — 5=deny-both closes it w/o LPAC, 0=still defeated)",
                launch(&exe, &["__child__", "read", &secret.to_string_lossy()], &exe_dir, ac_sid, false, false));
        }

        // ── VIABILITY E: launch a REAL program (node.exe) under LPAC. node lives in a dir
        //    that carries AAP (not necessarily ARAP), so under LPAC its own dir needs an
        //    AC-SID grant to load. Tests whether a real toolchain starts under LPAC.
        if let Some(node) = which("node.exe") {
            let node_dir = node.parent().unwrap().to_path_buf();
            add_ace(&node_dir, ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true).ok();
            report("E.std.node  (node -e exit0, standard AC — EXPECT 0)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, false, false));
            report("E.lpac.node (node -e exit0, LPAC + AC-SID grant on node dir — 0=viable, nonzero=start-fail)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, true, false));
            // LPAC without the node-dir grant: does node start purely on ARAP of system+its
            // own dir? (If node's dir lacks ARAP → start-fail. Shows the minimal grant need.)
            report("E.lpac.node.nogrant (LPAC, NO node-dir AC grant — probes whether node dir carries ARAP)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, true, true));
        } else {
            println!("E.node: node.exe not on PATH — skipped");
        }

        // Cleanup best-effort.
        let _ = unsafe { DeleteAppContainerProfile(to_wide(&name).as_ptr()) };
        println!("=== matrix complete ===");
    }

    fn report(case: &str, r: io::Result<u32>) {
        match r {
            Ok(code) => println!("CASE {case}: exit={code}"),
            Err(e) => println!("CASE {case}: LAUNCH-ERROR {e}"),
        }
    }

    // ── tree setup ───────────────────────────────────────────────────────────────
    fn mktree(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("lpacprobe_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // ── SID helpers ──────────────────────────────────────────────────────────────
    // Leak intentionally (throwaway probe, short-lived): each returns a raw PSID that
    // stays valid for the process lifetime.
    fn sid(s: &str) -> PSID {
        let wide = to_wide(s);
        let mut out: PSID = std::ptr::null_mut();
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut out) };
        assert!(ok != 0, "ConvertStringSidToSidW {s}");
        out
    }
    fn current_user_sid_ptr() -> PSID {
        let mut tok: HANDLE = std::ptr::null_mut();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) };
        let mut ret: u32 = 0;
        // TokenUser = 1. First size, then fetch.
        unsafe { GetTokenInformation(tok, 1, std::ptr::null_mut(), 0, &mut ret) };
        let mut buf = vec![0u8; ret as usize];
        let ok = unsafe {
            GetTokenInformation(tok, 1, buf.as_mut_ptr().cast(), ret, &mut ret)
        };
        assert!(ok != 0, "GetTokenInformation(TokenUser)");
        unsafe { CloseHandle(tok) };
        // TOKEN_USER { SID_AND_ATTRIBUTES { PSID Sid, u32 Attributes } } — first field is
        // the PSID. Leak the buffer so the SID stays valid.
        let sid_ptr = unsafe { *(buf.as_ptr() as *const PSID) };
        std::mem::forget(buf);
        sid_ptr
    }

    // ── ACL helpers ──────────────────────────────────────────────────────────────
    type AceSpec = (PSID, u32, i32, bool); // (sid, access, mode, inherit)

    /// Replace the object's DACL with EXACTLY these ACEs and PROTECT it (drop inherited).
    fn set_protected_dacl(path: &Path, aces: &[AceSpec]) {
        let mut eas: Vec<EXPLICIT_ACCESS_W> = aces.iter().map(|&(s, access, mode, inh)| ea(s, access, mode, inh)).collect();
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(eas.len() as u32, eas.as_mut_ptr(), std::ptr::null_mut(), &mut new_dacl) };
        assert_eq!(rc, 0, "SetEntriesInAclW protected");
        let wpath = to_wide_path(path);
        let rc = unsafe {
            SetNamedSecurityInfoW(
                wpath.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null_mut(),
            )
        };
        if !new_dacl.is_null() {
            unsafe { LocalFree(new_dacl.cast()) };
        }
        assert_eq!(rc, 0, "SetNamedSecurityInfoW protected {}", path.display());
    }

    /// Additively merge one ACE into the object's existing DACL (read-modify-write).
    fn add_ace(path: &Path, s: PSID, access: u32, mode: i32, inherit: bool) -> io::Result<()> {
        use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
        use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;
        let wpath = to_wide_path(path);
        let mut old: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = unsafe {
            GetNamedSecurityInfoW(wpath.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(), std::ptr::null_mut(), &mut old, std::ptr::null_mut(), &mut sd)
        };
        if rc != 0 { return Err(io::Error::from_raw_os_error(rc as i32)); }
        let mut e = ea(s, access, mode, inherit);
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(1, &mut e, old, &mut new_dacl) };
        if rc != 0 { unsafe { LocalFree(sd) }; return Err(io::Error::from_raw_os_error(rc as i32)); }
        let rc = unsafe {
            SetNamedSecurityInfoW(wpath.as_ptr() as *mut u16, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(), std::ptr::null_mut(), new_dacl, std::ptr::null_mut())
        };
        unsafe { if !new_dacl.is_null() { LocalFree(new_dacl.cast()); } LocalFree(sd); }
        if rc != 0 { return Err(io::Error::from_raw_os_error(rc as i32)); }
        Ok(())
    }

    fn ea(s: PSID, access: u32, mode: i32, inherit: bool) -> EXPLICIT_ACCESS_W {
        let mut e: EXPLICIT_ACCESS_W = unsafe { std::mem::zeroed() };
        e.grfAccessPermissions = access;
        e.grfAccessMode = mode;
        e.grfInheritance = if inherit { CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE } else { NO_INHERITANCE };
        e.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: s.cast(),
        };
        e
    }

    // ── AppContainer profile ─────────────────────────────────────────────────────
    fn create_appcontainer(name: &str) -> io::Result<PSID> {
        let w = to_wide(name);
        let mut sid: PSID = std::ptr::null_mut();
        // Delete a stale one first (idempotent across reruns).
        unsafe { DeleteAppContainerProfile(w.as_ptr()) };
        let hr = unsafe { CreateAppContainerProfile(w.as_ptr(), w.as_ptr(), w.as_ptr(), std::ptr::null(), 0, &mut sid) };
        if hr != 0 { return Err(io::Error::other(format!("CreateAppContainerProfile hr=0x{hr:08x}"))); }
        Ok(sid)
    }

    // ── the launch (standard AC or LPAC) ─────────────────────────────────────────
    fn launch(program: &Path, args: &[&str], cwd: &Path, ac_sid: PSID, lpac: bool, allow_internet: bool) -> io::Result<u32> {
        // internetClient capability iff requested (kept for symmetry; unused by fs cases).
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        if allow_internet {
            caps.push(SID_AND_ATTRIBUTES { Sid: sid(INTERNET_CLIENT_SID), Attributes: SE_GROUP_ENABLED });
        }
        let mut sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: ac_sid,
            Capabilities: if caps.is_empty() { std::ptr::null_mut() } else { caps.as_mut_ptr() },
            CapabilityCount: caps.len() as u32,
            Reserved: 0,
        };

        // Attribute list: SECURITY_CAPABILITIES, plus (if lpac) the AAP-policy opt-out.
        let n_attrs: u32 = if lpac { 2 } else { 1 };
        let mut size: usize = 0;
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), n_attrs, 0, &mut size) };
        let words = size.div_ceil(std::mem::size_of::<usize>()).max(1);
        let mut buf = vec![0usize; words];
        let attr = buf.as_mut_ptr().cast::<std::ffi::c_void>();
        if unsafe { InitializeProcThreadAttributeList(attr, n_attrs, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            UpdateProcThreadAttribute(attr, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                std::ptr::from_mut(&mut sec_caps).cast(), std::mem::size_of::<SECURITY_CAPABILITIES>(),
                std::ptr::null_mut(), std::ptr::null_mut())
        } == 0 {
            let e = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(attr) };
            return Err(e);
        }
        let mut policy: u32 = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
        if lpac
            && unsafe {
                UpdateProcThreadAttribute(attr, 0, PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
                    std::ptr::from_mut(&mut policy).cast(), std::mem::size_of::<u32>(),
                    std::ptr::null_mut(), std::ptr::null_mut())
            } == 0
        {
            let e = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(attr) };
            return Err(io::Error::other(format!("LPAC attribute set failed: {e}")));
        }

        let mut cmdline = build_command_line(program, args);
        let cwd_wide = to_wide(&cwd.to_string_lossy().replace('/', "\\"));

        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attr;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;

        let ok = unsafe {
            CreateProcessW(std::ptr::null(), cmdline.as_mut_ptr(), std::ptr::null(), std::ptr::null(),
                0, flags, std::ptr::null(), cwd_wide.as_ptr(), std::ptr::from_mut(&mut si).cast(), &mut pi)
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(attr) };
            return Err(e);
        }
        unsafe { ResumeThread(pi.hThread) };
        let code = unsafe {
            if WaitForSingleObject(pi.hProcess, INFINITE) != WAIT_OBJECT_0 {
                let e = io::Error::last_os_error();
                CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
                DeleteProcThreadAttributeList(attr);
                return Err(e);
            }
            let mut c: u32 = 0;
            GetExitCodeProcess(pi.hProcess, &mut c);
            CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
            DeleteProcThreadAttributeList(attr);
            c
        };
        Ok(code)
    }

    // ── small utils ──────────────────────────────────────────────────────────────
    fn which(exe: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let c = dir.join(exe);
            if c.is_file() { return Some(c); }
        }
        None
    }
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn to_wide_path(p: &Path) -> Vec<u16> {
        to_wide(&p.to_string_lossy().replace('/', "\\"))
    }
    fn build_command_line(program: &Path, args: &[&str]) -> Vec<u16> {
        let mut line: Vec<u16> = Vec::new();
        append_quoted(&mut line, program.as_os_str());
        for a in args {
            line.push(u16::from(b' '));
            append_quoted(&mut line, std::ffi::OsStr::new(a));
        }
        line.push(0);
        line
    }
    fn append_quoted(out: &mut Vec<u16>, arg: &std::ffi::OsStr) {
        let wide: Vec<u16> = arg.encode_wide().collect();
        let needs = wide.is_empty() || wide.iter().any(|&c| c == 32 || c == 9 || c == 34);
        if !needs { out.extend_from_slice(&wide); return; }
        out.push(34);
        let mut bs = 0usize;
        for &c in &wide {
            if c == 92 { bs += 1; }
            else if c == 34 { for _ in 0..(bs*2+1) { out.push(92); } out.push(34); bs = 0; }
            else { for _ in 0..bs { out.push(92); } bs = 0; out.push(c); }
        }
        for _ in 0..(bs*2) { out.push(92); }
        out.push(34);
    }
}
