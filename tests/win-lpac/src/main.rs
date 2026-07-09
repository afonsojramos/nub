//! LPAC vs standard AppContainer — empirical windows-latest probe (iteration 2).
//!
//! Questions:
//!  (1) Does an LPAC launch EXCLUDE the ALL APPLICATION PACKAGES SID (S-1-15-2-1) from
//!      the access check? — atomic A block (AAP-only file: std reads, LPAC denies).
//!  (2) Is deny-inside-a-broad-allow EXPRESSIBLE? — F block, a PROTECTED, explicitly
//!      CANONICALLY-ordered DACL [AC-SID deny, AC-SID allow, AAP allow] so ACE order is
//!      controlled (iteration-1's inherited-DACL cases were ordering-ambiguous).
//!  (3) Is LPAC VIABLE for a real toolchain? — E block launches node.exe under LPAC with
//!      escalating grants (leaf grant → + a Chromium-style LPAC capability set) to find
//!      the minimal grant that lets node initialize.
//!
//! Self-reexec `__child__ <role>`; exit-code contract: 0 read-ok, 5 access-denied,
//! 9 other-io-error, 21 is-appcontainer, 20 not. The runner prints a CASE matrix and
//! exits 0 on completion (a "confinement failed" line is DATA, not a harness failure).

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
        ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
        GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
    };
    use windows_sys::Win32::Security::{
        ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, DeriveCapabilitySidsFromName,
        GetTokenInformation, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
        SID_AND_ATTRIBUTES, TOKEN_QUERY, TokenIsAppContainer,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, OpenProcessToken,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const SET_ACCESS: i32 = 2;
    const DENY_ACCESS: i32 = 3;
    const NO_INHERITANCE: u32 = 0x0;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    // PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY = ProcThreadAttributeValue(15,
    // FALSE, TRUE, FALSE) = 15 | 0x20000 = 0x2000F.
    const PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY: usize = 0x0002_000F;
    const PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 0x1;
    const AAP_SID: &str = "S-1-15-2-1";
    const SE_GROUP_ENABLED: u32 = 0x4;

    // The Chromium/Edge LPAC capability set (by name; SIDs are derived at runtime). A
    // functional LPAC process typically needs these for CRT/winsock/registry init.
    const LPAC_CAPS: &[&str] = &[
        "registryRead",
        "lpacWebPlatform",
        "lpacCom",
        "lpacIdentityServices",
        "lpacCryptoServices",
        "lpacAppExperience",
        "lpacInstrumentation",
        "lpacServicesManagement",
        "lpacSessionManagement",
        "lpacDeviceAccess",
        "lpacPnpNotifications",
        "lpacClipboard",
        "lpacEnterprisePolicyChangeNotifications",
        "internetClient",
    ];

    pub fn child_main(a: &[String]) -> i32 {
        match a.first().map(String::as_str) {
            Some("read") => match std::fs::read(&a[1]) {
                Ok(_) => 0,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => 5,
                Err(_) => 9,
            },
            Some("token") => {
                let mut tok: HANDLE = std::ptr::null_mut();
                if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
                    return 20;
                }
                let mut is_ac: u32 = 0;
                let mut ret: u32 = 0;
                let ok = unsafe {
                    GetTokenInformation(tok, TokenIsAppContainer,
                        std::ptr::from_mut(&mut is_ac).cast(), 4, &mut ret)
                };
                unsafe { CloseHandle(tok) };
                if ok != 0 && is_ac != 0 { 21 } else { 20 }
            }
            _ => 99,
        }
    }

    pub fn run_matrix() {
        println!("=== win-lpac-probe matrix (iter2) ===");
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().unwrap().to_path_buf();

        let name = format!("nub_lpac_probe_{}", std::process::id());
        let ac_sid = match create_appcontainer(&name) {
            Ok(s) => s,
            Err(e) => { println!("FATAL create_appcontainer: {e}"); std::process::exit(2); }
        };
        if let Err(e) = add_ace(&exe_dir, ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true) {
            println!("WARN grant exe_dir: {e}");
        }
        let caps = derive_caps();
        println!("derived {} LPAC capability SIDs from {} names", caps.len(), LPAC_CAPS.len());

        report("T0.lpac.token (LPAC launches, token is AppContainer)",
            launch(&exe, &["__child__", "token"], &exe_dir, ac_sid, true, &[]));

        // ── A: pure AAP exclusion (the crux) ──
        {
            let dir = mktree("A_aaponly");
            let f = dir.join("f.txt");
            std::fs::write(&f, b"secret").unwrap();
            set_protected_dacl(&dir, &[
                owner_fc(),
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, SET_ACCESS, true),
            ]);
            report("A.std  (AAP-only file, std AC — EXPECT 0)",
                launch(&exe, &["__child__", "read", &f.to_string_lossy()], &exe_dir, ac_sid, false, &[]));
            report("A.lpac (AAP-only file, LPAC — EXPECT 5 if LPAC excludes AAP)",
                launch(&exe, &["__child__", "read", &f.to_string_lossy()], &exe_dir, ac_sid, true, &[]));
        }

        // ── F: deny-inside-allow with a CONTROLLED canonical DACL order ──
        // secret DACL (PROTECTED): [AC-SID deny, AC-SID allow, AAP allow]. pub: [AC-SID
        // allow, AAP allow]. Canonical order = deny first.
        {
            let dir = mktree("F_denyorder");
            let pubf = dir.join("pub.txt");
            let secret = dir.join("secret.txt");
            std::fs::write(&pubf, b"public").unwrap();
            std::fs::write(&secret, b"SECRET").unwrap();
            set_protected_dacl(&dir, &[owner_fc()]); // dir traversable by owner; children set below
            set_protected_dacl(&pubf, &[
                owner_fc(),
                (ac_sid, GENERIC_READ | GENERIC_EXECUTE, SET_ACCESS, false),
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, SET_ACCESS, false),
            ]);
            set_protected_dacl(&secret, &[
                owner_fc(),
                (ac_sid, GENERIC_READ | GENERIC_EXECUTE, DENY_ACCESS, false),
                (ac_sid, GENERIC_READ | GENERIC_EXECUTE, SET_ACCESS, false),
                (sid(AAP_SID), GENERIC_READ | GENERIC_EXECUTE, SET_ACCESS, false),
            ]);
            report("F.std.pub    (std AC — EXPECT 0)",
                launch(&exe, &["__child__", "read", &pubf.to_string_lossy()], &exe_dir, ac_sid, false, &[]));
            report("F.std.secret (deny AC-SID + allow AC-SID + allow AAP, std AC — 5=deny wins, 0=AAP defeats deny [TRAP])",
                launch(&exe, &["__child__", "read", &secret.to_string_lossy()], &exe_dir, ac_sid, false, &[]));
            report("F.lpac.pub    (LPAC — EXPECT 0)",
                launch(&exe, &["__child__", "read", &pubf.to_string_lossy()], &exe_dir, ac_sid, true, &[]));
            report("F.lpac.secret (LPAC, AAP inert — 5=deny-inside-allow EXPRESSIBLE, 0=not)",
                launch(&exe, &["__child__", "read", &secret.to_string_lossy()], &exe_dir, ac_sid, true, &[]));
        }

        // ── E: node.exe viability under LPAC ──
        if let Some(node) = which("node.exe") {
            let node_dir = node.parent().unwrap().to_path_buf();
            add_ace(&node_dir, ac_sid, GENERIC_READ | GENERIC_EXECUTE, GRANT_ACCESS, true).ok();
            report("E.std.node        (node -e exit0, std AC — EXPECT 0)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, false, &[]));
            report("E.lpac.node.leaf  (LPAC + node-dir grant, NO caps — iter1 gave winsock-fail 0x80000003)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, true, &[]));
            report("E.lpac.node.caps  (LPAC + node-dir grant + Chromium LPAC cap set — 0=viable-with-caps)",
                launch(&node, &["-e", "process.exit(0)"], &node_dir, ac_sid, true, &caps));
        } else {
            println!("E.node: node.exe not on PATH — skipped");
        }

        let _ = unsafe { DeleteAppContainerProfile(to_wide(&name).as_ptr()) };
        println!("=== matrix complete ===");
    }

    fn report(case: &str, r: io::Result<u32>) {
        match r {
            Ok(code) => println!("CASE {case}: exit={code}"),
            Err(e) => println!("CASE {case}: LAUNCH-ERROR {e}"),
        }
    }

    fn mktree(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("lpacprobe_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sid(s: &str) -> PSID {
        let wide = to_wide(s);
        let mut out: PSID = std::ptr::null_mut();
        assert!(unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut out) } != 0, "sid {s}");
        out
    }
    fn owner_fc() -> AceSpec {
        (current_user_sid_ptr(), 0x1F01FF, SET_ACCESS, true)
    }
    fn current_user_sid_ptr() -> PSID {
        let mut tok: HANDLE = std::ptr::null_mut();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) };
        let mut ret: u32 = 0;
        unsafe { GetTokenInformation(tok, 1, std::ptr::null_mut(), 0, &mut ret) };
        let mut buf = vec![0u8; ret as usize];
        assert!(unsafe { GetTokenInformation(tok, 1, buf.as_mut_ptr().cast(), ret, &mut ret) } != 0);
        unsafe { CloseHandle(tok) };
        let sid_ptr = unsafe { *(buf.as_ptr() as *const PSID) };
        std::mem::forget(buf);
        sid_ptr
    }

    /// Derive the CapabilitySids[0] for each LPAC capability name (SE_GROUP_ENABLED).
    fn derive_caps() -> Vec<PSID> {
        let mut out = Vec::new();
        for name in LPAC_CAPS {
            let w = to_wide(name);
            let mut grp: *mut PSID = std::ptr::null_mut();
            let mut grp_n: u32 = 0;
            let mut cap: *mut PSID = std::ptr::null_mut();
            let mut cap_n: u32 = 0;
            let ok = unsafe {
                DeriveCapabilitySidsFromName(w.as_ptr(), &mut grp, &mut grp_n, &mut cap, &mut cap_n)
            };
            if ok != 0 && cap_n > 0 && !cap.is_null() {
                let first = unsafe { *cap };
                out.push(first); // leak (throwaway probe)
            }
        }
        out
    }

    type AceSpec = (PSID, u32, i32, bool);

    fn set_protected_dacl(path: &Path, aces: &[AceSpec]) {
        let mut eas: Vec<EXPLICIT_ACCESS_W> = aces.iter().map(|&(s, a, m, i)| ea(s, a, m, i)).collect();
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(eas.len() as u32, eas.as_mut_ptr(), std::ptr::null_mut(), &mut new_dacl) };
        assert_eq!(rc, 0, "SetEntriesInAclW");
        let wpath = to_wide_path(path);
        let rc = unsafe {
            SetNamedSecurityInfoW(wpath.as_ptr() as *mut u16, SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(), std::ptr::null_mut(), new_dacl, std::ptr::null_mut())
        };
        if !new_dacl.is_null() { unsafe { LocalFree(new_dacl.cast()) }; }
        assert_eq!(rc, 0, "SetNamedSecurityInfoW {}", path.display());
    }

    fn add_ace(path: &Path, s: PSID, access: u32, mode: i32, inherit: bool) -> io::Result<()> {
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

    fn create_appcontainer(name: &str) -> io::Result<PSID> {
        let w = to_wide(name);
        let mut sid: PSID = std::ptr::null_mut();
        unsafe { DeleteAppContainerProfile(w.as_ptr()) };
        let hr = unsafe { CreateAppContainerProfile(w.as_ptr(), w.as_ptr(), w.as_ptr(), std::ptr::null(), 0, &mut sid) };
        if hr != 0 { return Err(io::Error::other(format!("hr=0x{hr:08x}"))); }
        Ok(sid)
    }

    fn launch(program: &Path, args: &[&str], cwd: &Path, ac_sid: PSID, lpac: bool, caps: &[PSID]) -> io::Result<u32> {
        let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = caps.iter()
            .map(|&s| SID_AND_ATTRIBUTES { Sid: s, Attributes: SE_GROUP_ENABLED }).collect();
        let mut sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: ac_sid,
            Capabilities: if cap_attrs.is_empty() { std::ptr::null_mut() } else { cap_attrs.as_mut_ptr() },
            CapabilityCount: cap_attrs.len() as u32,
            Reserved: 0,
        };

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
        if lpac && unsafe {
            UpdateProcThreadAttribute(attr, 0, PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
                std::ptr::from_mut(&mut policy).cast(), 4, std::ptr::null_mut(), std::ptr::null_mut())
        } == 0 {
            let e = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(attr) };
            return Err(io::Error::other(format!("LPAC attr: {e}")));
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
                CloseHandle(pi.hThread); CloseHandle(pi.hProcess); DeleteProcThreadAttributeList(attr);
                return Err(e);
            }
            let mut c: u32 = 0;
            GetExitCodeProcess(pi.hProcess, &mut c);
            CloseHandle(pi.hThread); CloseHandle(pi.hProcess); DeleteProcThreadAttributeList(attr);
            c
        };
        Ok(code)
    }

    fn which(exe: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|d| d.join(exe)).find(|c| c.is_file())
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
        for a in args { line.push(u16::from(b' ')); append_quoted(&mut line, std::ffi::OsStr::new(a)); }
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
