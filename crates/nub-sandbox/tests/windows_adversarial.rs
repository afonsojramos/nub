//! Windows AppContainer backend — ADVERSARIAL non-fs confinement probe (windows-latest
//! CI only). Companion to `windows_enforcement.rs`: that test proves each axis ENFORCES;
//! this one attacks the NON-fs axes + the broader AppContainer escape surface and reports
//! whether each attack is BLOCKED or OPEN. It drives the REAL backend (`apply` →
//! `Prepared::status` → `WindowsLaunch::run`) so every verdict is a property of nub's own
//! launch, not a reproduction.
//!
//! `harness = false`: the binary is BOTH runner and probe child — `__sbxadv__ <role>` acts
//! as the child (an exit-code contract), anything else runs the cases.
//!
//! This is an AUDIT probe (investigation-scope): it PRINTS a verdict per attack rather
//! than only pass/failing, so the maintainer sees the true state of each axis. A `FAIL`
//! here means an attack SUCCEEDED (a confinement hole); `PASS` means the attack was
//! blocked. Diagnostic lines are printed for the log; verdicts ride the exit-code contract.

#[cfg(not(target_os = "windows"))]
fn main() {
    // Non-Windows host: nothing to enforce. (`harness = false` needs a `main`.)
}

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__sbxadv__") {
        std::process::exit(win::child_main(&args[2..]));
    }
    match win::run_adversarial() {
        Ok(()) => println!("ALL WINDOWS ADVERSARIAL PROBES PASSED (no holes found)"),
        Err(n) => {
            eprintln!("{n} WINDOWS ADVERSARIAL PROBE(S) FAILED (holes / anomalies above)");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "windows")]
mod win {
    use nub_sandbox::policy::{
        CanonGlob, Effect, EnvPolicy, FsAccess, FsPolicy, FsRule, FsRuleSet, NetPolicy, PidPolicy,
        SandboxPolicy, TmpMode,
    };
    use nub_sandbox::{CommandSpec, Degradation, apply};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    // ── raw constants (avoid extra windows-sys feature deps for a few values) ──────
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const MEM_COMMIT: u32 = 0x1000;
    const PAGE_GUARD: u32 = 0x100;
    const PAGE_NOACCESS: u32 = 0x01;
    // CreateProcess breakaway flag.
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_SUSPENDED_F: u32 = 0x0000_0004;

    // ── the probe child ─────────────────────────────────────────────────────────

    /// Exit-code contract (read by the parent). Chosen so a denial is never confused with
    /// a crash. See each role for its specific codes.
    pub fn child_main(a: &[String]) -> i32 {
        match a.first().map(String::as_str) {
            // Read a needle out of ANOTHER process's memory via OpenProcess(VM_READ)+scan.
            //   0 = needle recovered (VECTOR OPEN, secret exfiltrated)
            //   2 = OpenProcess(VM_READ) succeeded but needle not located (gate still OPEN)
            //   5 = OpenProcess(VM_READ) DENIED (gate closed — safe)
            //   9 = other error
            Some("readenv") => read_foreign_env(&a[1], &a[2]),
            // Dump this token's privileges; 20 if any DANGEROUS privilege is present.
            Some("privs") => dump_privileges(),
            // Count capability SIDs (S-1-15-3-*) in this token; exit = the count (capped 90).
            Some("caps") => count_capabilities(),
            // Attempt to break out of the Job via CREATE_BREAKAWAY_FROM_JOB.
            //   0  = breakaway DENIED (good)   21 = breakaway SUCCEEDED (escape; pid → marker)
            Some("breakaway") => attempt_breakaway(&a[1]),
            // Record own pid to a marker then sleep (a live target for the readers).
            Some("sleepmark") => {
                if let Some(m) = a.get(1) {
                    let _ = std::fs::write(m, std::process::id().to_string());
                }
                std::thread::sleep(Duration::from_secs(70));
                0
            }
            Some("sleep") => {
                std::thread::sleep(Duration::from_secs(70));
                0
            }
            // UDP egress to an external host under no-internetClient.
            //   5 = blocked (WSAEACCES)   0 = send/connect OK (LEAK)   6 = timeout   9 = other
            Some("udpx") => udp_egress(&a[1], a[2].parse().unwrap_or(0)),
            // Open a well-known local named pipe.
            //   0 = OPENED (IPC reach)   5 = access-denied   2 = not-found/busy   9 = other
            Some("pipe") => open_named_pipe(&a[1]),
            // Attempt ReadFile on a raw (numeric) handle value the parent held open, and
            // verify the bytes contain the secret marker (a stale handle value could COLLIDE
            // with an unrelated child handle — the content check makes a "leak" genuine).
            //   0 = read the secret via inherited handle (LEAK)  7 = invalid/denied/wrong-object
            Some("usehandle") => use_raw_handle(&a[1], &a[2]),
            _ => 2,
        }
    }

    /// OpenProcess(PROCESS_VM_READ) on `pid` and scan its committed memory for `needle`
    /// (UTF-16LE — the env block encoding). The OpenProcess result is the GATE; the scan is
    /// the proof of actual data recovery.
    fn read_foreign_env(pid_s: &str, needle: &str) -> i32 {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQueryEx};
        use windows_sys::Win32::System::Threading::OpenProcess;

        let Ok(pid) = pid_s.parse::<u32>() else {
            return 9;
        };
        // SAFETY: OpenProcess with query-only access rights; a denied open returns null.
        let h = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            let e = std::io::Error::last_os_error();
            // Retry with query-only to distinguish "can't open at all" from "no VM_READ".
            let h2 = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if h2.is_null() {
                println!("CHILD readenv OpenProcess FULLY DENIED err={:?}", e.raw_os_error());
            } else {
                println!(
                    "CHILD readenv OpenProcess(VM_READ) DENIED but QUERY ok err={:?}",
                    e.raw_os_error()
                );
                unsafe { CloseHandle(h2) };
            }
            return 5;
        }
        println!("CHILD readenv OpenProcess(VM_READ) SUCCEEDED on pid {pid} — gate OPEN");

        // Needle as UTF-16LE bytes (env block is wide).
        let needle_w: Vec<u8> = needle
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();

        let mut addr: usize = 0;
        let mut scanned: u64 = 0;
        let cap: u64 = 512 * 1024 * 1024; // don't scan more than 512MB
        let mut found = false;
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        while scanned < cap {
            // SAFETY: query the region at `addr`; 0 return = end of address space.
            let n = unsafe {
                VirtualQueryEx(
                    h,
                    addr as *const _,
                    &mut mbi,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if n == 0 {
                break;
            }
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            let readable = mbi.State == MEM_COMMIT
                && (mbi.Protect & PAGE_GUARD) == 0
                && (mbi.Protect & PAGE_NOACCESS) == 0
                && mbi.Protect != 0;
            if readable && size > 0 {
                // Read the region in chunks, tolerating partial/failed reads.
                let chunk = 64 * 1024usize;
                let mut off = 0usize;
                let mut carry: Vec<u8> = Vec::new();
                while off < size {
                    let want = chunk.min(size - off);
                    let mut buf = vec![0u8; want];
                    let mut got: usize = 0;
                    let ok = unsafe {
                        ReadProcessMemory(
                            h,
                            (base + off) as *const _,
                            buf.as_mut_ptr().cast(),
                            want,
                            &mut got,
                        )
                    };
                    if ok != 0 && got > 0 {
                        buf.truncate(got);
                        // Search carry+buf so a needle straddling a chunk boundary is caught.
                        let mut hay = std::mem::take(&mut carry);
                        hay.extend_from_slice(&buf);
                        if contains(&hay, &needle_w) {
                            found = true;
                            break;
                        }
                        // Keep the last (needle-1) bytes for boundary straddling.
                        let keep = needle_w.len().saturating_sub(1);
                        if hay.len() > keep {
                            carry = hay[hay.len() - keep..].to_vec();
                        } else {
                            carry = hay;
                        }
                        scanned += got as u64;
                    }
                    off += want;
                    if got == 0 {
                        break;
                    }
                }
            }
            if found {
                break;
            }
            let next = base.wrapping_add(size);
            if next <= addr {
                break;
            }
            addr = next;
        }
        unsafe { CloseHandle(h) };
        if found {
            println!("CHILD readenv RECOVERED the needle from pid {pid}'s memory");
            0
        } else {
            println!("CHILD readenv VM_READ open but needle not located (gate still OPEN)");
            2
        }
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || hay.len() < needle.len() {
            return false;
        }
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// Dump the child's token privileges; return 20 if any privilege from the dangerous set
    /// is present, else 0.
    fn dump_privileges() -> i32 {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{
            GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeNameW, TOKEN_PRIVILEGES,
            TOKEN_QUERY, TokenPrivileges,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        const DANGEROUS: &[&str] = &[
            "SeDebugPrivilege",
            "SeImpersonatePrivilege",
            "SeAssignPrimaryTokenPrivilege",
            "SeTcbPrivilege",
            "SeLoadDriverPrivilege",
            "SeBackupPrivilege",
            "SeRestorePrivilege",
            "SeTakeOwnershipPrivilege",
            "SeCreateTokenPrivilege",
            "SeManageVolumePrivilege",
            "SeRelabelPrivilege",
        ];

        // SAFETY: standard token-query sequence; the buffer is sized by the first call.
        unsafe {
            let mut tok = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return 9;
            }
            let mut len = 0u32;
            GetTokenInformation(tok, TokenPrivileges, std::ptr::null_mut(), 0, &mut len);
            if len == 0 {
                CloseHandle(tok);
                return 9;
            }
            let mut buf = vec![0u8; len as usize];
            if GetTokenInformation(tok, TokenPrivileges, buf.as_mut_ptr().cast(), len, &mut len) == 0
            {
                CloseHandle(tok);
                return 9;
            }
            CloseHandle(tok);
            let tp = &*(buf.as_ptr() as *const TOKEN_PRIVILEGES);
            let count = tp.PrivilegeCount as usize;
            let arr = std::slice::from_raw_parts(
                std::ptr::addr_of!(tp.Privileges) as *const LUID_AND_ATTRIBUTES,
                count,
            );
            let mut dangerous = false;
            for la in arr {
                let mut name = [0u16; 128];
                let mut nlen = name.len() as u32;
                let luid = la.Luid;
                if LookupPrivilegeNameW(std::ptr::null(), &luid, name.as_mut_ptr(), &mut nlen) != 0 {
                    let s = String::from_utf16_lossy(&name[..nlen as usize]);
                    let flag = if DANGEROUS.contains(&s.as_str()) {
                        dangerous = true;
                        " <<DANGEROUS>>"
                    } else {
                        ""
                    };
                    println!("CHILD priv {s} attrs=0x{:x}{flag}", la.Attributes);
                }
            }
            if dangerous { 20 } else { 0 }
        }
    }

    /// Count capability SIDs (S-1-15-3-*) in the token's groups; exit = count (capped 90).
    fn count_capabilities() -> i32 {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{
            GetSidIdentifierAuthority, GetSidSubAuthority, GetTokenInformation, SID_AND_ATTRIBUTES,
            TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        // SAFETY: token-group enumeration; buffer sized by the first call.
        unsafe {
            let mut tok = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return 90;
            }
            let mut len = 0u32;
            GetTokenInformation(tok, TokenGroups, std::ptr::null_mut(), 0, &mut len);
            if len == 0 {
                CloseHandle(tok);
                return 90;
            }
            let mut buf = vec![0u8; len as usize];
            if GetTokenInformation(tok, TokenGroups, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
                CloseHandle(tok);
                return 90;
            }
            CloseHandle(tok);
            let tg = &*(buf.as_ptr() as *const TOKEN_GROUPS);
            let count = tg.GroupCount as usize;
            let arr = std::slice::from_raw_parts(
                std::ptr::addr_of!(tg.Groups) as *const SID_AND_ATTRIBUTES,
                count,
            );
            let mut caps = 0i32;
            for g in arr {
                let sid = g.Sid;
                if sid.is_null() {
                    continue;
                }
                // Capability SID: identifier authority 15 (SECURITY_APP_PACKAGE_AUTHORITY)
                // AND first subauthority == 3 (SECURITY_CAPABILITY_BASE_RID).
                let ia = GetSidIdentifierAuthority(sid);
                if ia.is_null() {
                    continue;
                }
                let auth5 = (*ia).Value[5];
                let sub0 = *GetSidSubAuthority(sid, 0);
                if auth5 == 15 && sub0 == 3 {
                    let sub1 = *GetSidSubAuthority(sid, 1);
                    println!("CHILD capability SID S-1-15-3-{sub1}");
                    caps += 1;
                }
            }
            caps.min(90)
        }
    }

    /// Attempt to spawn a sleeper OUTSIDE the Job via CREATE_BREAKAWAY_FROM_JOB. On success,
    /// record the escapee's pid to `marker` (the parent then checks it outlives the Job).
    fn attempt_breakaway(marker: &str) -> i32 {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Threading::{
            CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
        };
        let Ok(exe) = std::env::current_exe() else {
            return 9;
        };
        // Command line: "<exe>" __sbxadv__ sleep
        let mut cl: Vec<u16> = Vec::new();
        cl.push(u16::from(b'"'));
        cl.extend(exe.as_os_str().encode_wide());
        cl.push(u16::from(b'"'));
        for a in [" __sbxadv__ sleep"] {
            cl.extend(a.encode_utf16());
        }
        cl.push(0);
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: attempt a breakaway spawn; cl outlives the call.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cl.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_BREAKAWAY_FROM_JOB | CREATE_SUSPENDED_F,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::from_mut(&mut si).cast(),
                &mut pi,
            )
        };
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            // 5 = ERROR_ACCESS_DENIED = the Job's no-breakaway policy denied it (the
            // expected block). Any OTHER failure is inconclusive (couldn't even attempt),
            // not a proven block — keep the two distinct so a masked failure isn't read
            // as "breakaway safely denied."
            if e.raw_os_error() == Some(5) {
                println!("CHILD breakaway DENIED (ERROR_ACCESS_DENIED — Job no-breakaway policy)");
                return 0;
            }
            println!("CHILD breakaway CreateProcess failed err={:?} (inconclusive)", e.raw_os_error());
            return 22;
        }
        // Breakaway succeeded — record the pid, resume it so it lives, and report escape.
        let pid = pi.dwProcessId;
        let _ = std::fs::write(marker, pid.to_string());
        unsafe {
            windows_sys::Win32::System::Threading::ResumeThread(pi.hThread);
            windows_sys::Win32::Foundation::CloseHandle(pi.hThread);
            windows_sys::Win32::Foundation::CloseHandle(pi.hProcess);
        }
        println!("CHILD breakaway SUCCEEDED — escapee pid {pid} spawned outside the Job");
        21
    }

    /// UDP datagram egress to an external host under no-internetClient. Connected UDP so the
    /// WFP connect-authorization fires; a blocked send/connect surfaces WSAEACCES (10013).
    fn udp_egress(ip: &str, port: u16) -> i32 {
        use std::net::UdpSocket;
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
            return 9;
        };
        let addr = format!("{ip}:{port}");
        if let Err(e) = sock.connect(&addr) {
            return classify_wsa(e);
        }
        match sock.send(b"nub-adv-probe") {
            Ok(_) => {
                println!("CHILD udpx send OK to {addr} (egress NOT blocked)");
                0
            }
            Err(e) => classify_wsa(e),
        }
    }

    fn classify_wsa(e: std::io::Error) -> i32 {
        match e.raw_os_error() {
            Some(10013) => {
                println!("CHILD udpx WSAEACCES (blocked)");
                5
            }
            Some(other) => {
                println!("CHILD udpx err os={other}");
                9
            }
            None => 9,
        }
    }

    /// Open a well-known local named pipe by name (`\\.\pipe\<name>`).
    fn open_named_pipe(name: &str) -> i32 {
        let path = format!("\\\\.\\pipe\\{name}");
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(_) => {
                println!("CHILD pipe OPENED {path} (IPC reach)");
                0
            }
            Err(e) => match e.raw_os_error() {
                Some(5) => {
                    println!("CHILD pipe ACCESS DENIED {path}");
                    5
                }
                // 2 = file-not-found, 231 = all-pipe-instances-busy (exists, reachable)
                Some(2) => {
                    println!("CHILD pipe NOT FOUND {path}");
                    2
                }
                Some(231) => {
                    println!("CHILD pipe BUSY (exists+reachable) {path}");
                    0
                }
                Some(o) => {
                    println!("CHILD pipe err os={o} {path}");
                    9
                }
                None => 9,
            },
        }
    }

    /// Attempt ReadFile on a raw numeric handle value the parent held open (a file handle to
    /// the secret). If the handle-list scoping worked, the value is invalid in the child.
    fn use_raw_handle(hex: &str, needle: &str) -> i32 {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        let Ok(val) = usize::from_str_radix(hex.trim_start_matches("0x"), 16) else {
            return 9;
        };
        let h = val as HANDLE;
        let mut buf = [0u8; 128];
        let mut read = 0u32;
        // SAFETY: ReadFile on a numeric handle; an un-inherited value fails (no deref of ours).
        let ok = unsafe {
            ReadFile(
                h,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 && read > 0 {
            let got = &buf[..read as usize];
            if got.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                println!("CHILD usehandle READ the SECRET via inherited handle (LEAK)");
                return 0;
            }
            // Read succeeded on some OTHER object (handle-value collision) — not a leak.
            println!("CHILD usehandle read {read} bytes but NOT the secret (handle-value collision, no leak)");
            return 7;
        }
        let e = std::io::Error::last_os_error();
        println!("CHILD usehandle ReadFile failed err={:?} (handle not usable)", e.raw_os_error());
        7
    }

    // ── liveness + kill helpers ─────────────────────────────────────────────────────

    fn is_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION as PQLI,
        };
        // SAFETY: open by pid for query only; STILL_ACTIVE (259) ⇒ alive.
        unsafe {
            let h = OpenProcess(PQLI, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == 259
        }
    }

    fn kill(pid: u32) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        // SAFETY: best-effort cleanup of a target/escapee.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }

    // ── the fixture ───────────────────────────────────────────────────────────────

    struct Fixture {
        root: PathBuf,
        child: PathBuf,
        work: PathBuf,
        secret: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = PathBuf::from(format!("C:\\nub-adv-{nonce:x}"));
            let bin = root.join("bin");
            let work = root.join("work");
            let vault = root.join("vault");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::create_dir_all(&work).unwrap();
            std::fs::create_dir_all(&vault).unwrap();
            let child = bin.join("child.exe");
            std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
            let secret = vault.join("secret.env");
            std::fs::write(&secret, b"TOPSECRET_FILE=do-not-leak").unwrap();
            Fixture {
                root,
                child,
                work,
                secret,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn canon(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }
    fn native(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    // ── policy builders ─────────────────────────────────────────────────────────────

    fn rule(p: &Path, effect: Effect, access: FsAccess) -> FsRule {
        FsRule {
            matcher: CanonGlob(canon(p)),
            effect,
            access,
        }
    }

    fn read_confine(read: &[&Path], write: &[&Path]) -> SandboxPolicy {
        let mut entries = Vec::new();
        for r in read {
            entries.push(rule(r, Effect::Allow, FsAccess::Read));
        }
        for w in write {
            entries.push(rule(w, Effect::Allow, FsAccess::ReadWrite));
        }
        SandboxPolicy {
            fs: FsPolicy {
                rules: FsRuleSet {
                    entries,
                    default_effect: Effect::Deny,
                },
                tmp: TmpMode::Private,
            },
            net: NetPolicy::default(),
            env: EnvPolicy::default(),
            pid: PidPolicy::default(),
        }
    }

    /// The Windows-essential env baseline (no secret). Mirrors windows_enforcement's base_env
    /// so a scrubbed AppContainer child can still start (CreateProcessW resolves per-container
    /// storage from the passed env).
    fn base_env(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for k in [
            "SystemRoot",
            "SystemDrive",
            "windir",
            "ComSpec",
            "PATHEXT",
            "Path",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "APPDATA",
            "LOCALAPPDATA",
            "ProgramData",
            "ALLUSERSPROFILE",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "PUBLIC",
            "USERNAME",
            "USERDOMAIN",
            "COMPUTERNAME",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
            "OS",
            "DriverData",
        ] {
            if let Ok(v) = std::env::var(k) {
                m.insert(k.to_string(), v);
            }
        }
        for (k, v) in extra {
            m.insert(k.to_string(), v.to_string());
        }
        m
    }

    /// A scrubbing env policy: enforce, constructed baseline (no secret), the named secret
    /// withheld — the shape that triggers the `env-read-ascendant` degradation.
    fn scrub_env(withheld: &str) -> EnvPolicy {
        EnvPolicy {
            enforce: true,
            constructed: base_env(&[]),
            schema: Vec::new(),
            withheld: vec![withheld.to_string()],
        }
    }

    // ── run helpers ─────────────────────────────────────────────────────────────────

    /// Run a policy over the child, returning the achieved Degradation + the child exit code.
    fn run(policy: &SandboxPolicy, program: &Path, args: &[&str]) -> (Degradation, i32) {
        let spec = CommandSpec::new(program.as_os_str()).args(args.iter().copied());
        match apply(policy, spec) {
            Ok(p) => {
                let deg = p.degradation.clone();
                match p.status() {
                    Ok(s) => (deg, s.code().unwrap_or(-1)),
                    Err(e) => {
                        eprintln!("  [status Err] {e} os={:?}", e.raw_os_error());
                        (deg, -101)
                    }
                }
            }
            Err(d) => {
                eprintln!("  [apply Err] {d:?}");
                (d, -100)
            }
        }
    }

    /// Launch the child on a background thread (for a live target). Returns immediately.
    fn spawn_bg(policy: SandboxPolicy, program: PathBuf, args: Vec<String>) {
        std::thread::spawn(move || {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let _ = run(&policy, &program, &refs);
        });
    }

    /// Poll a marker file for a pid (a target writing its own id), up to ~12s.
    fn await_pid(marker: &Path) -> Option<u32> {
        for _ in 0..120 {
            if let Ok(s) = std::fs::read_to_string(marker) {
                if let Ok(pid) = s.trim().parse::<u32>() {
                    return Some(pid);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    // ── verdict recording ───────────────────────────────────────────────────────────
    // PASS = the attack was BLOCKED (safe). FAIL = the attack SUCCEEDED (a hole). Each is a
    // named line so the CI log reads as an audit result, not a bare assertion.

    struct Report {
        fails: u32,
    }
    impl Report {
        fn blocked_if(&mut self, label: &str, attack_open: bool, detail: &str) {
            if attack_open {
                self.fails += 1;
                eprintln!("FAIL(HOLE) {label}: attack SUCCEEDED — {detail}");
            } else {
                println!("PASS(blocked) {label}: {detail}");
            }
        }
        fn note(&self, label: &str, detail: &str) {
            println!("NOTE {label}: {detail}");
        }
    }

    // ── the cases ─────────────────────────────────────────────────────────────────

    pub fn run_adversarial() -> Result<(), u32> {
        let f = Fixture::new();
        let child = f.child.clone();
        let mut r = Report { fails: 0 };
        let a = |s: &str| s.to_string();

        // Base confinement used by most cases (fs read-confine → engages the AppContainer).
        let confine = read_confine(&[&f.work], &[]);

        // ── 1. PROCESS_VM_READ: nub's OWN env (the documented reduced case) ──────────
        // Env-scrub ENFORCED so the secret is only in the PARENT (this harness) memory, not
        // the child's own env. The child then tries to recover it from the parent.
        let parent_needle = format!("advparentsecret{}", std::process::id());
        // SAFETY: single-threaded test main at this point; seed the ambient secret.
        unsafe { std::env::set_var("NUB_ADV_PARENT_SECRET", &parent_needle) };
        let mut own = read_confine(&[&f.work], &[]);
        own.env = scrub_env("NUB_ADV_PARENT_SECRET");
        let (deg_own, code_own) = run(
            &own,
            &child,
            &["__sbxadv__", "readenv", &a(&std::process::id().to_string()), &parent_needle],
        );
        // Honesty check: the degradation MUST name env-read-ascendant when the scrub withholds.
        let reports_ascendant = deg_own.lost.iter().any(|s| s == "env-read-ascendant");
        r.blocked_if(
            "env-read-ascendant-honesty (degradation names the residual)",
            !reports_ascendant,
            &format!("degradation.lost={:?}", deg_own.lost),
        );
        // code 5 = gate closed (VM_READ denied); 0/2 = gate open (documented residual real).
        r.note(
            "vm-read-nub-own-env (documented reduced case)",
            &format!(
                "child exit {code_own} ({})",
                match code_own {
                    0 => "RECOVERED secret from nub's memory — residual REAL",
                    2 => "OpenProcess(VM_READ) OPEN, needle not located — residual REAL (gate open)",
                    5 => "OpenProcess(VM_READ) DENIED — residual does NOT reproduce (doc may overstate)",
                    _ => "inconclusive",
                }
            ),
        );

        // ── 2. PROCESS_VM_READ: a SIBLING AppContainer child's env ──────────────────
        // A sibling LowBox child (its OWN unique AC SID) holds a secret in its constructed
        // env. Can a second LowBox child read the sibling's memory?
        let sib_marker = f.work.join("sib.pid");
        let sib_needle = "advsiblingsecret9a1b";
        let sib_env = base_env(&[("NUB_ADV_SIB_SECRET", sib_needle)]);
        let mut sib_policy = read_confine(&[&f.work], &[&f.work]);
        sib_policy.env = EnvPolicy {
            enforce: true,
            constructed: sib_env,
            schema: Vec::new(),
            withheld: Vec::new(),
        };
        spawn_bg(
            sib_policy,
            child.clone(),
            vec![
                "__sbxadv__".into(),
                "sleepmark".into(),
                native(&sib_marker),
            ],
        );
        if let Some(sib_pid) = await_pid(&sib_marker) {
            let (_d, code_sib) = run(
                &confine,
                &child,
                &["__sbxadv__", "readenv", &a(&sib_pid.to_string()), sib_needle],
            );
            r.blocked_if(
                "vm-read-SIBLING-appcontainer-env (cross-LowBox memory read)",
                code_sib == 0 || code_sib == 2,
                &format!("child exit {code_sib} (0/2 = sibling memory readable = HOLE, 5 = blocked)"),
            );
            kill(sib_pid);
        } else {
            r.note("vm-read-SIBLING", "INCONCLUSIVE — sibling target never registered a pid");
        }

        // ── 3. PROCESS_VM_READ: an ARBITRARY same-user Medium-IL process (runner-class) ─
        // A plain (non-AppContainer) same-user process holding a secret — the stand-in for
        // the CI runner agent / any other same-user process on the box. If the LowBox child
        // can scrape it, the residual is far broader than "only nub's own env".
        let arb_marker = f.work.join("arb.pid");
        let arb_needle = "advrunnersecret5c2d";
        let arb = std::process::Command::new(&child)
            .args(["__sbxadv__", "sleepmark", &native(&arb_marker)])
            .env("NUB_ADV_ARB_SECRET", arb_needle)
            .spawn();
        match arb {
            Ok(mut ch) => {
                if let Some(arb_pid) = await_pid(&arb_marker) {
                    let (_d, code_arb) = run(
                        &confine,
                        &child,
                        &["__sbxadv__", "readenv", &a(&arb_pid.to_string()), arb_needle],
                    );
                    r.blocked_if(
                        "vm-read-ARBITRARY-same-user-process (runner-env class)",
                        code_arb == 0 || code_arb == 2,
                        &format!(
                            "child exit {code_arb} (0/2 = arbitrary same-user memory readable = HIGH-severity HOLE, 5 = blocked)"
                        ),
                    );
                    kill(arb_pid);
                } else {
                    r.note("vm-read-ARBITRARY", "INCONCLUSIVE — target never registered a pid");
                }
                let _ = ch.kill();
                let _ = ch.wait();
            }
            Err(e) => r.note("vm-read-ARBITRARY", &format!("INCONCLUSIVE — spawn failed: {e}")),
        }

        // ── 4. JOB escape via CREATE_BREAKAWAY_FROM_JOB ──────────────────────────────
        // The backend's Job is KILL_ON_JOB_CLOSE with NO BREAKAWAY_OK, so a breakaway spawn
        // should be denied. If it succeeds, the escapee outlives the Job teardown.
        let bo_marker = f.work.join("breakaway.pid");
        let wc = read_confine(&[&f.work], &[&f.work]);
        let (_d, code_bo) = run(
            &wc,
            &child,
            &["__sbxadv__", "breakaway", &native(&bo_marker)],
        );
        // After run() returns, the Job handle has been closed (teardown). A broken-away
        // escapee would be OUTSIDE the Job and still alive.
        let escapee_alive = std::fs::read_to_string(&bo_marker)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|pid| {
                std::thread::sleep(Duration::from_millis(600));
                let alive = is_alive(pid);
                if alive {
                    kill(pid);
                }
                alive
            })
            .unwrap_or(false);
        r.blocked_if(
            "job-escape-breakaway (CREATE_BREAKAWAY_FROM_JOB)",
            code_bo == 21 || escapee_alive,
            &format!("child exit {code_bo} (21 = breakaway allowed), escapee-alive-after-close={escapee_alive}"),
        );

        // ── 5. Token privileges — no dangerous privilege beyond the benign set ───────
        let (_d, code_pr) = run(&confine, &child, &["__sbxadv__", "privs"]);
        r.blocked_if(
            "token-privileges (no dangerous privilege in the LowBox token)",
            code_pr == 20,
            &format!("child exit {code_pr} (20 = a dangerous privilege is present; see the CHILD priv lines)"),
        );

        // ── 6. Capability leak — 0 caps net-confined; exactly internetClient unconfined ─
        let mut net_deny = read_confine(&[&f.work], &[]);
        net_deny.net = NetPolicy {
            enforce: true,
            rules: Vec::new(),
            default_effect: Effect::Deny,
        };
        let (_d, caps_confined) = run(&net_deny, &child, &["__sbxadv__", "caps"]);
        r.blocked_if(
            "capabilities-net-confined (no capability SID granted)",
            caps_confined != 0,
            &format!("capability-SID count={caps_confined} (expect 0 under coarse net-deny)"),
        );
        // NC: net unconfined → the backend grants internetClient (exactly one capability).
        let (_d, caps_open) = run(&confine, &child, &["__sbxadv__", "caps"]);
        r.note(
            "capabilities-net-unconfined (NC)",
            &format!("capability-SID count={caps_open} (expect 1 = internetClient)"),
        );

        // ── 7. Coarse egress completeness — UDP external ────────────────────────────
        // NOTE (not an auto-FAIL): a WSAEACCES(5) on the connected-UDP connect is a clean
        // "blocked"; a send-success(0) is AMBIGUOUS (fire-and-forget can queue locally even
        // if WFP drops the datagram), so it is surfaced for review rather than auto-flagged.
        let (_d, code_udp) = run(
            &net_deny,
            &child,
            &["__sbxadv__", "udpx", "1.1.1.1", "53"],
        );
        r.note(
            "coarse-egress-udp",
            &format!(
                "child exit {code_udp} ({})",
                match code_udp {
                    5 => "WSAEACCES — UDP egress BLOCKED (good)",
                    6 => "timeout",
                    0 => "connect+send OK — REVIEW (possible UDP egress reach)",
                    _ => "other/err",
                }
            ),
        );

        // ── 8. Coarse egress — local named-pipe RPC reach ───────────────────────────
        // A LowBox that can OPEN a privileged RPC pipe has a local IPC/broker channel. Most
        // should be denied or ungranted. Each is a NOTE (reach ≠ automatic escape) but an
        // opened privileged pipe is surfaced for review.
        for pipe in ["epmapper", "lsass", "ntsvcs", "srvsvc", "InitShutdown", "atsvc"] {
            let (_d, code_pipe) = run(&net_deny, &child, &["__sbxadv__", "pipe", pipe]);
            let verdict = match code_pipe {
                0 => "OPENED (IPC reach — review)",
                5 => "access-denied",
                2 => "not-found",
                _ => "other/err",
            };
            if code_pipe == 0 {
                r.note(&format!("named-pipe-{pipe}"), &format!("exit {code_pipe}: {verdict}"));
            } else {
                r.note(&format!("named-pipe-{pipe}"), &format!("exit {code_pipe}: {verdict}"));
            }
        }

        // ── 9. Handle inheritance — a FILE handle to the secret is NOT usable in child ──
        // nub (this harness) holds an inheritable file handle to the secret open, then
        // launches the child through the real backend (which scopes inheritance to stdio via
        // PROC_THREAD_ATTRIBUTE_HANDLE_LIST). The child tries ReadFile on the raw value.
        if let Some((raw_hex, guard)) = open_inheritable_file(&f.secret) {
            let (_d, code_h) = run(
                &confine,
                &child,
                &["__sbxadv__", "usehandle", &raw_hex, "TOPSECRET_FILE"],
            );
            r.blocked_if(
                "handle-inheritance-file (secret file handle not inherited)",
                code_h == 0,
                &format!("child exit {code_h} (0 = READ the secret via inherited handle = LEAK, 7 = not usable)"),
            );
            drop(guard);
        } else {
            r.note("handle-inheritance-file", "INCONCLUSIVE — could not open an inheritable handle");
        }

        // ── degradation honesty: per-host net degrades to coarse-deny, reported ──────
        let mut per_host = read_confine(&[&f.work], &[]);
        per_host.net = NetPolicy {
            enforce: true,
            rules: vec![nub_sandbox::policy::NetRule {
                target: nub_sandbox::policy::NetTarget::Host("example.com".to_string()),
                effect: Effect::Allow,
            }],
            default_effect: Effect::Deny,
        };
        let (deg_ph, _c) = run(
            &per_host,
            &child,
            &["__sbxadv__", "udpx", "1.1.1.1", "53"],
        );
        r.blocked_if(
            "net-per-host-honesty (degradation names the un-wired per-host)",
            !deg_ph.lost.iter().any(|s| s == "net-per-host"),
            &format!("degradation.lost={:?}", deg_ph.lost),
        );

        if r.fails == 0 { Ok(()) } else { Err(r.fails) }
    }

    /// Open an inheritable read handle to `path`; returns its numeric value (hex) + a guard
    /// that closes it on drop. Marks it inheritable so it WOULD inherit under a naive
    /// bInheritHandles=TRUE spawn — the backend's handle-list is what must exclude it.
    fn open_inheritable_file(path: &Path) -> Option<(String, HandleGuard)> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING,
        };
        let wpath: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: open the secret for read; INVALID on failure.
        let h = unsafe {
            CreateFileW(
                wpath.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        Some((format!("0x{:x}", h as usize), HandleGuard(h)))
    }

    struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}
