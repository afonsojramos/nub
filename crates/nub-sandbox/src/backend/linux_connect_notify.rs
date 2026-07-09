//! Closes the port-scoped `ConnectTcp` residual in [`NetMode::Proxy`](super::linux)
//! with a seccomp USER_NOTIF supervisor over `connect()`.
//!
//! THE RESIDUAL: Landlock `ConnectTcp` pins egress to the proxy PORT, not its
//! ADDRESS (Landlock has no address filter; classic seccomp can't deref the
//! `connect()` sockaddr pointer). So in-sandbox code can `connect()` straight to
//! `external_ip:proxy_port` and skip the SNI gate.
//!
//! THE CLOSE: a 2nd seccomp filter (installed via raw `seccomp(…, NEW_LISTENER)` in
//! the child's pre_exec — unprivileged under NNP) routes `connect()` to a USER_NOTIF.
//! The nub PARENT holds the listener fd (passed child→parent over a socketpair via
//! SCM_RIGHTS) and runs a supervisor thread that, per notification, dereferences the
//! child's sockaddr from `/proc/<pid>/mem` and permits ONLY `127.0.0.1:<proxy_port>`.
//!
//! TOCTOU-ROBUST ALLOW (crun/gVisor pattern): a naive SECCOMP_USER_NOTIF_FLAG_CONTINUE
//! allow lets the child rewrite the sockaddr between our read and the kernel's connect.
//! Instead, on ALLOW the SUPERVISOR owns the connect — it connects a fresh socket to
//! the FIXED `127.0.0.1:<proxy_port>` (never the child-supplied, re-readable address)
//! and injects it over the child's socket fd via `NOTIF_ADDFD` (SETFD). Since the only
//! permitted destination IS that constant, a post-read rewrite changes nothing; the
//! DENY path is final regardless of a rewrite. Both directions are airtight.
//!
//! FAIL-CLOSED: a mem-read failure denies that connect; if every copy of the listener
//! fd closes (supervisor gone), the kernel returns ENOSYS to notified connects.
//!
//! CAPABILITY FLOOR: unchanged. USER_NOTIF (≥5.0), NOTIF_ADDFD (≥5.9) both sit below
//! the Landlock-v4 (6.7) floor that Proxy mode already requires. Only viability gate:
//! the supervisor reads the child's memory, which yama `ptrace_scope ≥ 2` forbids even
//! for a parent — on such hosts we skip the supervisor and keep the (documented,
//! bounded) residual rather than break per-host egress.
#![cfg(target_os = "linux")]

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── seccomp / ioctl ABI (asm-generic _IOC; correct for x86_64 + aarch64, nub's only
//    Linux targets) ────────────────────────────────────────────────────────────────
const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_ADDFD_FLAG_SETFD: u32 = 1;
const SECCOMP_IOC_MAGIC: libc::c_ulong = b'!' as libc::c_ulong;
const IOC_WRITE: libc::c_ulong = 1;
const IOC_READ: libc::c_ulong = 2;

/// asm-generic `_IOC(dir, type, nr, size)` — the layout shared by x86_64/aarch64.
const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: libc::c_ulong) -> libc::c_ulong {
    (dir << 30) | (size << 16) | (SECCOMP_IOC_MAGIC << 8) | nr
}
const NOTIF_RECV: libc::c_ulong = ioc(
    IOC_READ | IOC_WRITE,
    0,
    mem::size_of::<SeccompNotif>() as libc::c_ulong,
);
const NOTIF_SEND: libc::c_ulong = ioc(
    IOC_READ | IOC_WRITE,
    1,
    mem::size_of::<SeccompNotifResp>() as libc::c_ulong,
);
const NOTIF_ID_VALID: libc::c_ulong = ioc(IOC_WRITE, 2, mem::size_of::<u64>() as libc::c_ulong);
const NOTIF_ADDFD: libc::c_ulong = ioc(
    IOC_WRITE,
    3,
    mem::size_of::<SeccompNotifAddfd>() as libc::c_ulong,
);

// classic-BPF opcodes for the tiny connect→USER_NOTIF filter.
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

// AUDIT_ARCH_* for the filter's arch guard. Only x86_64/aarch64 are supported; on any
// other Linux arch `viable()` returns false so this sentinel is never assembled in.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const AUDIT_ARCH: u32 = 0;

#[repr(C)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}
#[repr(C)]
struct SeccompNotif {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}
#[repr(C)]
struct SeccompNotifResp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}
#[repr(C)]
struct SeccompNotifAddfd {
    id: u64,
    flags: u32,
    srcfd: u32,
    newfd: u32,
    newfd_flags: u32,
}

// The ioctl request numbers hard-depend on these exact layouts (kernel UAPI).
const _: () = assert!(mem::size_of::<SeccompNotif>() == 80);
const _: () = assert!(mem::size_of::<SeccompNotifResp>() == 24);
const _: () = assert!(mem::size_of::<SeccompNotifAddfd>() == 24);

/// Whether the connect-notify supervisor can run on this host: a supported arch AND
/// yama `ptrace_scope ≤ 1` (a parent can read a direct child's `/proc/<pid>/mem`).
/// Scope ≥ 2 forbids the read even for a parent, so the supervisor could only ever
/// deny — we skip it there and keep the bounded port-scoped residual instead.
pub(crate) fn viable() -> bool {
    if !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return false;
    }
    match std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope") {
        // No yama LSM → no ptrace restriction → readable.
        Err(_) => true,
        Ok(s) => s.trim().parse::<i32>().map(|v| v <= 1).unwrap_or(false),
    }
}

/// Parent-side handle carried from `apply` to `status`: both ends of the socketpair
/// (the parent end receives the listener fd after spawn) + the proxy port the
/// supervisor pins egress to. The child end is held only for RAII close — the forked
/// child uses+closes its own inherited copy in pre_exec; owning it here guarantees the
/// parent's copy is released even if `Prepared` is dropped without `status()`.
pub(crate) struct ConnectNotify {
    parent_sock: OwnedFd,
    _child_sock: OwnedFd,
    proxy_port: u16,
}

impl ConnectNotify {
    pub(crate) fn new(parent_sock: OwnedFd, child_sock: OwnedFd, proxy_port: u16) -> Self {
        Self {
            parent_sock,
            _child_sock: child_sock,
            proxy_port,
        }
    }

    /// Replace `command.status()` on the Proxy path: spawn (pre_exec installs the notify
    /// filter and hands us the listener fd), receive that fd, run the supervisor for the
    /// child's whole lifetime, wait, then tear it down. The [`EgressProxy`](super) stays
    /// owned by the caller's `Prepared` across this call.
    pub(crate) fn run(self, command: &mut Command) -> io::Result<ExitStatus> {
        let mut child = command.spawn()?;
        // The pre_exec sent the listener fd before execve; it waits in the socketpair
        // buffer (SCM_RIGHTS keeps it alive even though the child already closed its copy).
        let notify_fd = match recv_listener_fd(self.parent_sock.as_raw_fd()) {
            Ok(fd) => fd,
            Err(e) => {
                // No listener → notified connects would ENOSYS (fail-closed) and the
                // child still runs; surface the error but don't strand the child.
                let _ = child.wait();
                return Err(e);
            }
        };
        let port = self.proxy_port;
        let shutdown = Arc::new(AtomicBool::new(false));
        let sh = shutdown.clone();
        let handle = match std::thread::Builder::new()
            .name("nub-connect-notify".into())
            .spawn(move || supervise(notify_fd, port, &sh))
        {
            Ok(h) => h,
            Err(e) => {
                // The supervisor didn't start; its dropped closure closes `notify_fd`, so
                // the child's notified connects fail-closed (ENOSYS). Reap the child rather
                // than orphan it, then surface the error.
                let _ = child.wait();
                return Err(e);
            }
        };
        let status = child.wait();
        // The child is gone (POLLHUP already ended the loop in the common case); the flag
        // guarantees exit even if POLLHUP is missed.
        shutdown.store(true, Ordering::SeqCst);
        let _ = handle.join();
        status
    }
}

/// The `AF_UNIX` socketpair that carries the listener fd child→parent. `SOCK_CLOEXEC`
/// so neither end leaks into the exec'd program (the child end is used, then closed, in
/// pre_exec before execve). Returns `(parent, child)` owned ends; the caller captures
/// their raw fds into the pre_exec closure (which closes the forked child's copies).
pub(crate) fn make_socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: standard socketpair; `fds` is a valid 2-element buffer.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two fresh, owned fds.
    let parent = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let child = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((parent, child))
}

/// pre_exec (CHILD, post-fork/pre-exec): install the connect→USER_NOTIF filter, send its
/// listener fd to the parent over `child_sock`, then close both socketpair ends. Any
/// failure aborts the spawn (fail-closed). Async-signal-safe: only raw syscalls over
/// stack buffers, no allocation on the success path.
pub(crate) fn install_and_handoff(child_sock: RawFd, parent_sock: RawFd) -> io::Result<()> {
    // SAFETY: the child never needs the parent end; close it so it can't linger.
    unsafe { libc::close(parent_sock) };
    let notify_fd = install_notify_filter()?;
    send_fd(child_sock, notify_fd)?;
    // SAFETY: both fds are ours; the filter is installed in the kernel and the listener
    // fd is now in-flight to the parent, so the child's copies are done.
    unsafe {
        libc::close(notify_fd);
        libc::close(child_sock);
    }
    Ok(())
}

/// Install a seccomp filter that routes `connect()` to a USER_NOTIF and allows all else,
/// guarded on the native arch (a foreign-ABI syscall is left to the sibling seccompiler
/// filter, which KILLs it — higher precedence than this ALLOW). Requires NNP already set;
/// needs no privilege. Returns the NEW_LISTENER fd.
fn install_notify_filter() -> io::Result<RawFd> {
    const OFF_NR: u32 = 0;
    const OFF_ARCH: u32 = 4; // offsetof(struct seccomp_data, arch)
    let prog = [
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARCH),
        jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 0, 3),
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_NR),
        jump(BPF_JMP | BPF_JEQ | BPF_K, libc::SYS_connect as u32, 0, 1),
        stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
        stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    ];
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut _,
    };
    // SAFETY: raw seccomp(2); `fprog` points at a valid, live BPF program for the call.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &fprog as *const _,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd as RawFd)
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Send `fd` to the peer over a unix socket via a single-byte SCM_RIGHTS message.
fn send_fd(sock: RawFd, fd: RawFd) -> io::Result<()> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut _,
        iov_len: 1,
    };
    let mut cmsg = [0u8; 32];
    // SAFETY: msghdr is fully initialized below; the cmsg buffer is large enough for one
    // fd (CMSG_SPACE(4)); all pointers reference live local storage for the call.
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr() as *mut _;
        msg.msg_controllen = libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) as _;
        let hdr = libc::CMSG_FIRSTHDR(&msg);
        (*hdr).cmsg_level = libc::SOL_SOCKET;
        (*hdr).cmsg_type = libc::SCM_RIGHTS;
        (*hdr).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(&fd, libc::CMSG_DATA(hdr) as *mut RawFd, 1);
        loop {
            if libc::sendmsg(sock, &msg, 0) >= 0 {
                break;
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
    }
    Ok(())
}

/// Receive one fd sent via SCM_RIGHTS, returning it as an [`OwnedFd`].
fn recv_listener_fd(sock: RawFd) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut _,
        iov_len: 1,
    };
    let mut cmsg = [0u8; 32];
    // SAFETY: msghdr fully initialized; cmsg buffer sized for one fd; pointers are live
    // for the call. The received control message is validated (level/type) before use.
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr() as *mut _;
        msg.msg_controllen = cmsg.len() as _;
        loop {
            if libc::recvmsg(sock, &mut msg, 0) >= 0 {
                break;
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        let hdr = libc::CMSG_FIRSTHDR(&msg);
        if hdr.is_null()
            || (*hdr).cmsg_level != libc::SOL_SOCKET
            || (*hdr).cmsg_type != libc::SCM_RIGHTS
        {
            return Err(io::Error::other("no SCM_RIGHTS fd in handoff message"));
        }
        let mut fd: RawFd = -1;
        std::ptr::copy_nonoverlapping(libc::CMSG_DATA(hdr) as *const RawFd, &mut fd, 1);
        if fd < 0 {
            return Err(io::Error::other("invalid fd in handoff message"));
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// The supervisor loop. Owns `notify_fd` for the child's lifetime; exits on the child's
/// exit (POLLHUP) or the shutdown flag. Every notification: read the child's sockaddr,
/// re-validate the notif id (pid-reuse TOCTOU), then ALLOW only `127.0.0.1:<proxy_port>`
/// (supervisor-owned connect + ADDFD) else EPERM. A panic here unwinds `notify_fd` closed
/// → notified connects fail-closed with ENOSYS, so the child is never stranded.
fn supervise(notify_fd: OwnedFd, proxy_port: u16, shutdown: &AtomicBool) {
    let fd = notify_fd.as_raw_fd();
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single valid pollfd; 200ms timeout so the shutdown flag is observed.
        let pr = unsafe { libc::poll(&mut pfd, 1, 200) };
        if pr < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if pr == 0 {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            continue;
        }
        // Child gone → the filter's last user dropped; nothing left to service.
        if pfd.revents & libc::POLLHUP != 0 {
            return;
        }
        if pfd.revents & libc::POLLIN == 0 {
            continue;
        }
        if !handle_one(fd, proxy_port) {
            return;
        }
    }
}

/// Service one notification. Returns `false` on a fatal recv error (end the loop);
/// `true` otherwise (incl. a skipped/denied request).
fn handle_one(notify_fd: RawFd, proxy_port: u16) -> bool {
    // SAFETY: zeroed POD; NOTIF_RECV fills it. Kernel writes exactly sizeof(SeccompNotif).
    let mut req: SeccompNotif = unsafe { mem::zeroed() };
    if unsafe { libc::ioctl(notify_fd, NOTIF_RECV as _, &mut req) } != 0 {
        // ENOENT (the notifying syscall was interrupted/left) and EINTR (a signal hit the
        // supervisor thread) are transient — keep serving. Any other error is fatal.
        return matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOENT) | Some(libc::EINTR)
        );
    }
    let pid = req.pid;
    let child_sockfd = req.data.args[0] as u32;
    let sockaddr_ptr = req.data.args[1];

    let allowed = read_dest(pid, sockaddr_ptr)
        // Re-validate the notif id AFTER the mem read: if invalid (pid reused/target
        // gone), the read may be another process's memory — discard the decision.
        .filter(|_| unsafe { libc::ioctl(notify_fd, NOTIF_ID_VALID as _, &req.id) } == 0)
        .map(|(ip, port)| ip == [127, 0, 0, 1] && port == proxy_port)
        // A failed read (or a stale id) is fail-closed: deny.
        .unwrap_or(false);

    if allowed && inject_proxy_connect(notify_fd, &req, pid, child_sockfd, proxy_port) {
        return true;
    }
    respond(notify_fd, req.id, -libc::EPERM);
    true
}

/// Read the child's connect destination `(ipv4 octets, port)` from `/proc/<pid>/mem`.
/// `None` on any read/parse failure (→ fail-closed deny). We never trust this address
/// for the actual connect — only for the allow/deny decision.
fn read_dest(pid: u32, addr: u64) -> Option<([u8; 4], u16)> {
    use std::os::unix::fs::FileExt;
    let f = std::fs::File::open(format!("/proc/{pid}/mem")).ok()?;
    let mut buf = [0u8; mem::size_of::<libc::sockaddr_in>()];
    f.read_exact_at(&mut buf, addr).ok()?;
    // SAFETY: buf is exactly sizeof(sockaddr_in); read_unaligned tolerates any alignment.
    let sa: libc::sockaddr_in = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const _) };
    if sa.sin_family as i32 != libc::AF_INET {
        return None;
    }
    Some((sa.sin_addr.s_addr.to_ne_bytes(), u16::from_be(sa.sin_port)))
}

/// TOCTOU-robust ALLOW: connect a fresh socket to the FIXED loopback proxy, match the
/// child socket's `O_NONBLOCK`/`O_CLOEXEC`, inject it over the child's fd via ADDFD
/// (SETFD), then respond success. Returns `false` if any step fails (caller denies).
fn inject_proxy_connect(
    notify_fd: RawFd,
    req: &SeccompNotif,
    pid: u32,
    child_sockfd: u32,
    proxy_port: u16,
) -> bool {
    // SAFETY: fresh AF_INET stream socket; the connect target is the fixed loopback proxy.
    let up = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if up < 0 {
        return false;
    }
    let mut sa: libc::sockaddr_in = unsafe { mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = proxy_port.to_be();
    sa.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
    // SAFETY: `up` is a valid socket; `sa` is a fully-initialized sockaddr_in.
    let cr = unsafe {
        libc::connect(
            up,
            &sa as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if cr != 0 {
        unsafe { libc::close(up) };
        return false;
    }
    // Match the child's blocking mode so the injected fd behaves as the child expects
    // (Node/libuv drive non-blocking sockets); connect completed blocking above.
    let (nonblock, cloexec) = child_fd_flags(pid, child_sockfd);
    if nonblock {
        // SAFETY: `up` is ours; set O_NONBLOCK on the connected socket.
        unsafe {
            let fl = libc::fcntl(up, libc::F_GETFL);
            libc::fcntl(up, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    let mut addfd = SeccompNotifAddfd {
        id: req.id,
        flags: SECCOMP_ADDFD_FLAG_SETFD,
        srcfd: up as u32,
        newfd: child_sockfd,
        newfd_flags: if cloexec { libc::O_CLOEXEC as u32 } else { 0 },
    };
    // SAFETY: ADDFD installs `up` at the child's fd number (dup-like, same OFD). On
    // success we close our copy; the child keeps the connection.
    let ar = unsafe { libc::ioctl(notify_fd, NOTIF_ADDFD as _, &mut addfd) };
    unsafe { libc::close(up) };
    if ar < 0 {
        return false;
    }
    respond(notify_fd, req.id, 0)
}

/// Read the child fd's `O_NONBLOCK`/`O_CLOEXEC` from `/proc/<pid>/fdinfo/<fd>` (`flags:`
/// is octal `f_flags`). Absent/unparsed → both false (a benign blocking, non-cloexec fd).
fn child_fd_flags(pid: u32, fd: u32) -> (bool, bool) {
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else {
        return (false, false);
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("flags:")
            && let Ok(flags) = i64::from_str_radix(rest.trim(), 8)
        {
            return (
                flags & libc::O_NONBLOCK as i64 != 0,
                flags & libc::O_CLOEXEC as i64 != 0,
            );
        }
    }
    (false, false)
}

/// Complete a notification with `error` (0 = success). Best-effort: a failed send just
/// leaves the connect to time out / the child to exit.
fn respond(notify_fd: RawFd, id: u64, error: i32) -> bool {
    let mut resp = SeccompNotifResp {
        id,
        val: 0,
        error,
        flags: 0,
    };
    // SAFETY: NOTIF_SEND reads exactly sizeof(SeccompNotifResp) from `resp`.
    unsafe { libc::ioctl(notify_fd, NOTIF_SEND as _, &mut resp) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ioctl request numbers are load-bearing (a wrong _IOC silently mis-drives the
    // kernel). Pin them to the kernel's documented seccomp values.
    #[test]
    fn ioctl_numbers_match_uapi() {
        assert_eq!(NOTIF_RECV, 0xc050_2100);
        assert_eq!(NOTIF_SEND, 0xc018_2101);
        assert_eq!(NOTIF_ID_VALID, 0x4008_2102);
        assert_eq!(NOTIF_ADDFD, 0x4018_2103);
    }

    #[test]
    fn viable_is_bounded_by_arch() {
        // On the supported arches viability tracks ptrace_scope; the call must not panic
        // regardless of host yama config.
        let _ = viable();
    }
}
