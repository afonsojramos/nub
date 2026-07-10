//! Linux per-host egress — REAL enforcement e2e (Landlock v4 + seccomp + the live
//! egress proxy). Real-kernel only (Lima/Colima Ubuntu 24.04 locally, `ubuntu-24.04`
//! in CI); a no-Landlock kernel skips.
//!
//! Proves, each with a NEGATIVE CONTROL:
//!   - through the proxy: an allowed SNI FORWARDS, a denied SNI is DROPPED;
//!   - a DIRECT connect bypassing the proxy is BLOCKED (Landlock v4 pins connect to
//!     the proxy port; seccomp bans AF_INET entirely under coarse deny);
//!   - the AF_UNIX local-IPC egress channel (docker.sock class) is CLOSED under
//!     net-deny (`socket(AF_UNIX)` denied) — WITHOUT breaking node's fork IPC, which
//!     rides `socketpair(AF_UNIX)` (kept allowed). This is the empirical check that
//!     decided the AF_UNIX approach.
#![cfg(target_os = "linux")]

use nub_sandbox::compiler::{CompileCtx, ShellRunner};
use nub_sandbox::matcher::Homes;
use nub_sandbox::{CommandSpec, apply, compile};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::TempDir;

/// Landlock ABI level (0 if the syscall is unavailable). ABI>=2 = fs/net rulesets,
/// ABI>=4 = `ConnectTcp` (per-host egress).
fn landlock_abi() -> libc::c_long {
    const SYS: libc::c_long = 444;
    let abi = unsafe { libc::syscall(SYS, std::ptr::null::<libc::c_void>(), 0usize, 1u64) };
    abi.max(0)
}

/// True when the caller should SKIP (Landlock ABI below `min`). Under a truthy
/// `NUB_SANDBOX_REQUIRE_LANDLOCK` — the conformance real-kernel leg, which runs this
/// suite — a missing capability PANICS instead, so a hollow skip can't read as green.
fn skip_without_landlock(min: libc::c_long) -> bool {
    if landlock_abi() >= min {
        return false;
    }
    assert!(
        !require_landlock(),
        "NUB_SANDBOX_REQUIRE_LANDLOCK set but Landlock ABI < {min} — proxy conformance \
         cannot be proven on this kernel (real-kernel gate)"
    );
    true
}

/// A truthy `NUB_SANDBOX_REQUIRE_LANDLOCK` (`1`/`true`/`yes`).
fn require_landlock() -> bool {
    matches!(
        std::env::var("NUB_SANDBOX_REQUIRE_LANDLOCK").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// yama `ptrace_scope` (0 if the LSM is absent → unrestricted). The connect-notify
/// supervisor reads a direct-child's `/proc/<pid>/mem`, which needs scope ≤ 1.
fn ptrace_scope() -> i32 {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while let Ok(n) = s.read(&mut b) {
                    if n == 0 || s.write_all(&b[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

/// A throwaway AF_UNIX listener (stands in for docker.sock). Accepts + drops.
fn unix_server(path: &Path) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            drop(conn);
        }
    });
}

struct Fixture {
    _tmp: TempDir,
    proj: PathBuf,
    home: PathBuf,
}
fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let proj = root.join("proj");
    let home = root.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    Fixture {
        _tmp: tmp,
        proj,
        home,
    }
}
impl Fixture {
    fn ctx(&self) -> CompileCtx {
        CompileCtx {
            homes: Homes {
                home: self.home.clone(),
                tmp: std::env::temp_dir(),
                cache: self.home.join(".cache"),
                project: self.proj.clone(),
            },
            cwd: self.proj.clone(),
            trusted: true,
            ambient_env: Default::default(),
            runner: Box::new(ShellRunner),
        }
    }
    fn run(&self, surface: Value, program: &str, args: &[&str]) -> i32 {
        let policy = compile(&surface, &self.ctx()).expect("compiles");
        let spec = CommandSpec::new(program)
            .args(args.iter().copied())
            .cwd(&self.proj);
        let prepared = apply(&policy, spec).expect("apply");
        prepared.status().expect("spawn").code().unwrap_or(-1)
    }
}

fn compile_probe(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("probe.c");
    std::fs::write(&src, PROBE_C).ok()?;
    let bin = dir.join("probe");
    let ok = std::process::Command::new("cc")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    ok.then_some(bin)
}

// rawconnect <ip> <port> 0=ok 1=fail; unixconnect <path> 0=ok 1=denied/fail;
// proxysni <tgt> <port> <sni> 0=forwarded 5=dropped 1=err; proxynoauth <tgt> <port>
// 4=refused-407 0=leaked 1=err. Reads HTTP_PROXY for the port AND the per-session token.
const PROBE_C: &str = r#"
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>
#ifndef MSG_FASTOPEN
#define MSG_FASTOPEN 0x20000000
#endif

static int dial(const char* ip, int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct timeval tv = { .tv_sec = 3, .tv_usec = 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    struct sockaddr_in a; memset(&a,0,sizeof a);
    a.sin_family=AF_INET; a.sin_port=htons(port); inet_pton(AF_INET, ip, &a.sin_addr);
    if (connect(fd,(struct sockaddr*)&a,sizeof a)!=0){close(fd);return -1;}
    return fd;
}
static int proxy_port(void){const char*p=getenv("HTTP_PROXY");if(!p)return 0;const char*c=strrchr(p,':');return c?atoi(c+1):0;}
/* Extract the per-session token from HTTP_PROXY (`http://<token>@127.0.0.1:<port>`). */
static int proxy_token(char* out,int cap){
    const char*p=getenv("HTTP_PROXY"); if(!p)return 0;
    const char*s=strstr(p,"//"); if(!s)return 0; s+=2;
    const char*at=strchr(s,'@'); if(!at)return 0;
    int n=(int)(at-s); if(n<=0||n>=cap)return 0; memcpy(out,s,n); out[n]=0; return 1;
}
static const char B64[]="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
static void b64enc(const unsigned char*in,int len,char*out){
    int o=0,i=0;
    for(;i+3<=len;i+=3){unsigned v=(in[i]<<16)|(in[i+1]<<8)|in[i+2];
        out[o++]=B64[(v>>18)&63];out[o++]=B64[(v>>12)&63];out[o++]=B64[(v>>6)&63];out[o++]=B64[v&63];}
    int rem=len-i;
    if(rem==1){unsigned v=in[i]<<16;out[o++]=B64[(v>>18)&63];out[o++]=B64[(v>>12)&63];out[o++]='=';out[o++]='=';}
    else if(rem==2){unsigned v=(in[i]<<16)|(in[i+1]<<8);out[o++]=B64[(v>>18)&63];out[o++]=B64[(v>>12)&63];out[o++]=B64[(v>>6)&63];out[o++]='=';}
    out[o]=0;
}
/* `Proxy-Authorization: Basic <b64(token:)>\r\n` into hdr; 1 if a token was present. */
static int auth_header(char* hdr,int cap){
    char tok[128]; if(!proxy_token(tok,sizeof tok)){hdr[0]=0;return 0;}
    char cred[160]; int n=snprintf(cred,sizeof cred,"%s:",tok);
    char enc[256]; b64enc((const unsigned char*)cred,n,enc);
    snprintf(hdr,cap,"Proxy-Authorization: Basic %s\r\n",enc); return 1;
}
static int build_hello(const char* sni, unsigned char* out){
    unsigned char body[1024]; int b=0;
    body[b++]=0x03;body[b++]=0x03; for(int i=0;i<32;i++)body[b++]=0; body[b++]=0;
    body[b++]=0x00;body[b++]=0x02;body[b++]=0x13;body[b++]=0x01; body[b++]=0x01;body[b++]=0x00;
    int slen=strlen(sni); int entry=1+2+slen; int extdata=2+entry; int exttotal=4+extdata;
    body[b++]=(exttotal>>8)&0xff;body[b++]=exttotal&0xff;
    body[b++]=0x00;body[b++]=0x00; body[b++]=(extdata>>8)&0xff;body[b++]=extdata&0xff;
    body[b++]=(entry>>8)&0xff;body[b++]=entry&0xff; body[b++]=0x00;
    body[b++]=(slen>>8)&0xff;body[b++]=slen&0xff; memcpy(body+b,sni,slen);b+=slen;
    int o=0; out[o++]=0x16;out[o++]=0x03;out[o++]=0x01; int hs=4+b;
    out[o++]=(hs>>8)&0xff;out[o++]=hs&0xff;
    out[o++]=0x01;out[o++]=(b>>16)&0xff;out[o++]=(b>>8)&0xff;out[o++]=b&0xff;
    memcpy(out+o,body,b);o+=b; return o;
}
static int proxy_connect(const char* tgt,int tport,int with_auth){
    int pp=proxy_port(); if(!pp)return -1;
    int fd=dial("127.0.0.1",pp); if(fd<0)return -1;
    char hdr[256]; hdr[0]=0; if(with_auth) auth_header(hdr,sizeof hdr);
    char req[512]; int n=snprintf(req,sizeof req,"CONNECT %s:%d HTTP/1.1\r\nHost: %s\r\n%s\r\n",tgt,tport,tgt,hdr);
    if(write(fd,req,n)!=n){close(fd);return -1;}
    char resp[512];int got=0;
    while(got<4||memcmp(resp+got-4,"\r\n\r\n",4)){int r=read(fd,resp+got,1);if(r<=0){close(fd);return -1;}got+=r;if(got>=(int)sizeof resp)break;}
    if(strncmp(resp,"HTTP/1.1 200",12)!=0){close(fd);return -2;}
    return fd;
}
int main(int argc,char**argv){
    if(argc<2)return 1;
    if(!strcmp(argv[1],"rawconnect")){int fd=dial(argv[2],atoi(argv[3]));if(fd<0)return 1;close(fd);return 0;}
    /* SOCK_DGRAM|SOCK_CLOEXEC (the realistic flagged form) — proves the seccomp type
       mask ignores the high flag bits and still catches the datagram type. */
    if(!strcmp(argv[1],"udpsocket")){int fd=socket(AF_INET,SOCK_DGRAM|SOCK_CLOEXEC,0);if(fd<0)return 1;close(fd);return 0;}
    if(!strcmp(argv[1],"tcpbind")){
        int fd=socket(AF_INET,SOCK_STREAM,0); if(fd<0)return 1;
        struct sockaddr_in a; memset(&a,0,sizeof a); a.sin_family=AF_INET;
        a.sin_port=htons(atoi(argv[2])); a.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
        if(bind(fd,(struct sockaddr*)&a,sizeof a)!=0){close(fd);return 1;} close(fd);return 0;
    }
    if(!strcmp(argv[1],"unixconnect")){
        int fd=socket(AF_UNIX,SOCK_STREAM,0); if(fd<0)return 1; /* socket() denied */
        struct sockaddr_un a; memset(&a,0,sizeof a); a.sun_family=AF_UNIX;
        strncpy(a.sun_path,argv[2],sizeof a.sun_path-1);
        if(connect(fd,(struct sockaddr*)&a,sizeof a)!=0){close(fd);return 1;} close(fd);return 0;
    }
    if(!strcmp(argv[1],"proxysni")){
        int fd=proxy_connect(argv[2],atoi(argv[3]),1); if(fd<0)return 1;
        unsigned char hello[1024];int hl=build_hello(argv[4],hello);
        if(write(fd,hello,hl)!=hl){close(fd);return 1;}
        unsigned char e[64]; int r=read(fd,e,sizeof e); close(fd); return (r>0)?0:5;
    }
    /* Like proxysni's CONNECT but WITHOUT the token: proves the per-session gate. A
       co-resident process lacking the token is refused (407). 4=refused (expected),
       0=leaked-through (a bug), 1=couldn't reach proxy. */
    if(!strcmp(argv[1],"proxynoauth")){
        int fd=proxy_connect(argv[2],atoi(argv[3]),0);
        if(fd==-2)return 4; if(fd<0)return 1; close(fd); return 0;
    }
    /* Create an AF_INET SOCK_STREAM socket with protocol argv[2] (e.g. IPPROTO_SCTP=132,
       IPPROTO_MPTCP=262, IPPROTO_TCP=6, default=0). 0=created (allowed), 2=EPERM/EACCES
       (denied at creation), 1=other errno (e.g. EPROTONOSUPPORT — kernel lacks it). */
    if(!strcmp(argv[1],"mksock")){
        int fd=socket(AF_INET,SOCK_STREAM,atoi(argv[2])); int e=errno;
        if(fd>=0){close(fd);return 0;}
        if(e==EACCES||e==EPERM)return 2;
        return 1;
    }
    /* TCP Fast Open initiates a connection via sendto(MSG_FASTOPEN) WITHOUT calling
       connect() — probing whether it dodges the connect-only enforcement. 0=sendto
       accepted (initiated), 2=EPERM/EACCES (blocked), 1=other errno. */
    if(!strcmp(argv[1],"tfo")){
        int pp=proxy_port(); if(!pp)return 1;
        int fd=socket(AF_INET,SOCK_STREAM,0); if(fd<0)return 1;
        struct timeval tv={.tv_sec=3,.tv_usec=0};
        setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&tv,sizeof tv);
        struct sockaddr_in a; memset(&a,0,sizeof a);
        a.sin_family=AF_INET; a.sin_port=htons(pp); inet_pton(AF_INET,argv[2],&a.sin_addr);
        char buf[1]={'x'};
        int r=sendto(fd,buf,1,MSG_FASTOPEN,(struct sockaddr*)&a,sizeof a); int e=errno; close(fd);
        if(r>=0)return 0;
        if(e==EACCES||e==EPERM)return 2;
        return 1;
    }
    /* Direct connect to argv[2] ON THE PROXY PORT (read from HTTP_PROXY): the
       connect-notify residual. 0=connected, 2=EPERM/EACCES (blocked by the
       supervisor), 1=other failure. */
    if(!strcmp(argv[1],"residual")){
        int pp=proxy_port(); if(!pp)return 1;
        int fd=socket(AF_INET,SOCK_STREAM,0); if(fd<0)return 1;
        struct timeval tv={.tv_sec=3,.tv_usec=0};
        setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&tv,sizeof tv);
        setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&tv,sizeof tv);
        struct sockaddr_in a; memset(&a,0,sizeof a);
        a.sin_family=AF_INET; a.sin_port=htons(pp); inet_pton(AF_INET,argv[2],&a.sin_addr);
        int r=connect(fd,(struct sockaddr*)&a,sizeof a); int e=errno; close(fd);
        if(r==0)return 0;
        if(e==EACCES||e==EPERM)return 2;
        return 1;
    }
    return 1;
}
"#;

fn net_policy() -> Value {
    json!({ "fs": true, "net": ["127.0.0.0/8", "*.allowed.example"] })
}

#[test]
fn per_host_proxy_forwards_allowed_drops_denied_blocks_direct() {
    if skip_without_landlock(4) {
        return; // per-host needs Landlock ABI v4
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let echo = echo_server();
    let port = echo.port().to_string();
    let probe = probe.to_str().unwrap();

    assert_eq!(
        f.run(
            net_policy(),
            probe,
            &["proxysni", "127.0.0.1", &port, "api.allowed.example"]
        ),
        0,
        "allowed SNI forwards through the proxy"
    );
    assert_eq!(
        f.run(
            net_policy(),
            probe,
            &["proxysni", "127.0.0.1", &port, "evil.example"]
        ),
        5,
        "denied SNI dropped by the proxy SNI gate"
    );
    // DIRECT connect to the echo upstream: Landlock v4 pins connect to the proxy port,
    // so a connect to any other port is denied.
    assert_eq!(
        f.run(
            net_policy(),
            probe,
            &["rawconnect", &echo.ip().to_string(), &port]
        ),
        1,
        "direct connect bypassing the proxy is blocked by Landlock v4"
    );
    // Negative control: net relaxed → the direct connect succeeds. `sandbox: false`
    // is the unambiguous fully-unjailed surface (a bare `{ fs: true }` now floors
    // net → seccomp would block the connect, defeating the control).
    assert_eq!(
        f.run(
            json!(false),
            probe,
            &["rawconnect", &echo.ip().to_string(), &port]
        ),
        0,
        "neg-control: unenforced net connects directly"
    );
}

#[test]
fn proxy_requires_the_per_session_token() {
    // Defense-in-depth: the loopback proxy is reachable (Landlock carves the proxy port)
    // but a caller WITHOUT the per-session token is refused (407). The `proxynoauth` mode
    // omits the token, standing in for a co-resident same-user process that never learned
    // it; the paired positive control (WITH the token) forwards.
    if skip_without_landlock(4) {
        return; // per-host needs Landlock ABI v4
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let echo = echo_server();
    let port = echo.port().to_string();
    let probe = probe.to_str().unwrap();

    assert_eq!(
        f.run(net_policy(), probe, &["proxynoauth", "127.0.0.1", &port]),
        4,
        "a tokenless CONNECT to an admitted target must be refused (407)"
    );
    assert_eq!(
        f.run(
            net_policy(),
            probe,
            &["proxysni", "127.0.0.1", &port, "api.allowed.example"]
        ),
        0,
        "the child's own token must still forward"
    );
}

#[test]
fn ssrf_metadata_target_dropped_even_when_policy_admits_it() {
    // SSRF guard: a policy that ADMITS the link-local /16 at gate 1 still cannot reach the
    // cloud-metadata endpoint — connect_upstream refuses `169.254.169.254`. The negative
    // control (an admitted loopback echo under the SAME policy forwards) proves this is a
    // targeted egress block, not a broken tunnel. (That the drop is the GUARD and not a
    // dead-IP timeout is proven deterministically by the connect_upstream unit test.)
    if skip_without_landlock(4) {
        return; // per-host needs Landlock ABI v4
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let echo = echo_server();
    let port = echo.port().to_string();
    let probe = probe.to_str().unwrap();
    let policy =
        json!({ "fs": true, "net": ["169.254.0.0/16", "127.0.0.0/8", "*.allowed.example"] });

    assert_eq!(
        f.run(
            policy.clone(),
            probe,
            &["proxysni", "169.254.169.254", "80", "api.allowed.example"]
        ),
        5,
        "metadata target must be dropped at connect even though gate 1 admits it"
    );
    assert_eq!(
        f.run(
            policy,
            probe,
            &["proxysni", "127.0.0.1", &port, "api.allowed.example"]
        ),
        0,
        "neg-control: an admitted loopback target still forwards under the same policy"
    );
}

#[test]
fn sctp_and_mptcp_egress_denied_in_proxy_mode() {
    // Landlock's ConnectTcp governs ONLY IPPROTO_TCP, so an AF_INET SOCK_STREAM socket
    // over SCTP or MPTCP passes the SOCK_STREAM narrowing yet dodges the connect hook —
    // arbitrary external egress (MPTCP is default-on and transparently falls back to TCP
    // against any server, a drop-in for a normal TCP connection). Proxy mode narrows the
    // socket() protocol to TCP only, so both are denied at creation (pure seccomp — holds
    // regardless of the connect-notify supervisor's viability). IPPROTO_SCTP=132,
    // IPPROTO_MPTCP=262, IPPROTO_TCP=6.
    if skip_without_landlock(4) {
        return; // proxy mode needs Landlock ABI v4
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let probe = probe.to_str().unwrap();
    assert_eq!(
        f.run(net_policy(), probe, &["mksock", "132"]),
        2,
        "SCTP stream socket must be denied at creation in proxy mode"
    );
    assert_eq!(
        f.run(net_policy(), probe, &["mksock", "262"]),
        2,
        "MPTCP stream socket must be denied at creation in proxy mode"
    );
    // Plain TCP (and the default protocol, which resolves to TCP for SOCK_STREAM) stay
    // allowed — the proxy path must keep working.
    assert_eq!(
        f.run(net_policy(), probe, &["mksock", "6"]),
        0,
        "plain TCP stream socket must still be allowed in proxy mode"
    );
    assert_eq!(
        f.run(net_policy(), probe, &["mksock", "0"]),
        0,
        "default-protocol stream socket must still be allowed in proxy mode"
    );
    // Negative control: MPTCP (default-on, reliably creatable) IS created when net is
    // unenforced — proving the block above is ours, not a kernel-support artifact.
    assert_eq!(
        // `sandbox: false` = fully unjailed (a bare `{ fs: true }` now floors net).
        f.run(json!(false), probe, &["mksock", "262"]),
        0,
        "neg-control: unenforced net leaves an MPTCP socket creatable"
    );
}

#[test]
fn tcp_fast_open_egress_denied_in_proxy_mode() {
    // TCP Fast Open — sendto(MSG_FASTOPEN) — initiates a TCP connection WITHOUT calling
    // connect(), so it dodges BOTH the connect-notify supervisor and Landlock's
    // connect-only ConnectTcp hook: a direct external-egress bypass on the proxy port
    // (VM-verified to reach an external host when unblocked). Proxy mode denies the
    // MSG_FASTOPEN flag wholesale on the send syscalls — legit proxy traffic uses a plain
    // connect()+write, never TFO — so the flag is a safe scalar seccomp can match (the
    // address it cannot). This is pure seccomp, so it holds regardless of the supervisor's
    // viability (no ptrace_scope guard needed).
    if skip_without_landlock(4) {
        return; // proxy mode needs Landlock ABI v4
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let probe = probe.to_str().unwrap();
    assert_eq!(
        f.run(net_policy(), probe, &["tfo", "192.0.2.1"]),
        2,
        "TFO to an external host on the proxy port must be EPERM'd in proxy mode"
    );
}

#[test]
fn direct_connect_to_external_on_proxy_port_is_denied() {
    // The connect-notify close: Landlock's ConnectTcp pins egress to the proxy PORT but
    // not its ADDRESS, so in-sandbox code could `connect()` straight to an external host
    // ON that port and skip the proxy. The seccomp USER_NOTIF supervisor closes it —
    // permitting ONLY 127.0.0.1:<proxy_port>. Same proxy port, two addresses: the
    // loopback proxy is allowed (the legit path), an external address is EPERM'd. This
    // ADDRESS-level distinction is exactly what the port-only Landlock rule cannot make.
    if skip_without_landlock(4) {
        return; // per-host (and thus the supervisor) needs Landlock ABI v4
    }
    if ptrace_scope() > 1 {
        return; // supervisor can't read the child's mem under a hardened ptrace_scope
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let probe = probe.to_str().unwrap();
    assert_eq!(
        f.run(net_policy(), probe, &["residual", "127.0.0.1"]),
        0,
        "connect to the loopback proxy on the proxy port is allowed (the legit path)"
    );
    assert_eq!(
        f.run(net_policy(), probe, &["residual", "192.0.2.1"]),
        2,
        "direct connect to an EXTERNAL host on the proxy port is EPERM'd (residual closed)"
    );
}

#[test]
fn udp_and_inbound_bind_denied_in_proxy_mode() {
    // Proxy mode keeps AF_INET STREAM (for the proxy) but must close the channels that
    // would dodge the TCP-only proxy gate: a UDP socket (DNS-tunnel/QUIC exfil) and an
    // explicit TCP bind() (the deliberate inbound-listener exfil channel). A bind-less
    // listen() autobind is a documented dominated residual, not asserted here.
    if skip_without_landlock(4) {
        return;
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let probe = probe.to_str().unwrap();
    // socket(AF_INET, SOCK_DGRAM) denied by seccomp under Proxy mode.
    assert_eq!(
        f.run(net_policy(), probe, &["udpsocket"]),
        1,
        "UDP socket must be denied in proxy mode (no datagram exfil)"
    );
    // explicit bind() to a TCP port denied by Landlock BindTcp under Proxy mode.
    assert_eq!(
        f.run(net_policy(), probe, &["tcpbind", "0"]),
        1,
        "explicit TCP bind() must be denied in proxy mode"
    );
    // Negative controls: net relaxed → both succeed. `sandbox: false` is the
    // unambiguous fully-unjailed surface (a bare `{ fs: true }` now floors net).
    assert_eq!(
        f.run(json!(false), probe, &["udpsocket"]),
        0,
        "neg-control: unenforced net creates a UDP socket"
    );
    assert_eq!(
        f.run(json!(false), probe, &["tcpbind", "0"]),
        0,
        "neg-control: unenforced net binds a TCP port"
    );
}

#[test]
fn af_unix_egress_closed_under_net_deny() {
    if skip_without_landlock(2) {
        return;
    }
    let f = fixture();
    let Some(probe) = compile_probe(&f.proj) else {
        return;
    };
    let probe = probe.to_str().unwrap();
    let sock = f.home.join("docker.sock");
    unix_server(&sock);
    let sock = sock.to_str().unwrap();

    // Under coarse net-deny, socket(AF_UNIX) is denied → the local-IPC egress channel
    // (docker.sock class) is closed. `fs: true` is NAMED (an unlisted axis floors to
    // deny-all, which would stop the probe from exec'ing at all).
    assert_eq!(
        f.run(
            json!({ "net": false, "fs": true }),
            probe,
            &["unixconnect", sock]
        ),
        1,
        "AF_UNIX connect to docker.sock must be denied under net-deny"
    );
    // Negative control: no net enforcement → the same AF_UNIX connect succeeds.
    // `sandbox: false` = fully unjailed (a bare `{ fs: true }` now floors net).
    assert_eq!(
        f.run(json!(false), probe, &["unixconnect", sock]),
        0,
        "neg-control: unenforced net reaches the unix socket"
    );
}

#[test]
fn node_fork_ipc_survives_net_deny() {
    // The decisive empirical check: denying socket(AF_UNIX) must NOT break node's fork
    // IPC, which rides socketpair(AF_UNIX) (kept allowed). Under net:false the outer
    // node spawns a child over an IPC channel and exchanges a message.
    if skip_without_landlock(2) {
        return;
    }
    let node = "/usr/local/bin/node";
    if !Path::new(node).exists() {
        return;
    }
    let f = fixture();
    let script = r#"
const {spawn}=require('child_process');
const child=spawn(process.execPath,['-e','process.on("message",m=>{if(m==="ping"){process.send("pong");}})'],
  {stdio:['inherit','inherit','inherit','ipc']});
let done=false;
child.on('message',m=>{ if(m==='pong'){ done=true; child.kill(); process.exit(0);} });
child.on('spawn',()=>child.send('ping'));
setTimeout(()=>process.exit(done?0:7), 5000);
"#;
    // net:false denies socket(AF_UNIX); socketpair(AF_UNIX) stays allowed → IPC works.
    let code = f.run(json!({ "fs": true, "net": false }), node, &["-e", script]);
    assert_eq!(
        code, 0,
        "node fork IPC (socketpair) must survive net-deny (got {code})"
    );
}

#[test]
fn nonblocking_client_reaches_proxy_under_connect_notify() {
    // The connect-notify supervisor owns an ALLOWED connect and injects a fresh socket
    // over the child's fd (NOTIF_ADDFD). It must preserve the child's O_NONBLOCK, or a
    // non-blocking client (Node/libuv) would be handed a blocking fd and stall its event
    // loop. This drives a real libuv client through the proxy under the supervisor: node
    // connects to the loopback proxy (non-blocking), CONNECTs to an allowed upstream, and
    // must read the proxy's `200` — proving the injected fd behaves non-blocking.
    if skip_without_landlock(4) {
        return; // per-host (and the supervisor) needs Landlock ABI v4
    }
    if ptrace_scope() > 1 {
        return; // supervisor non-viable under a hardened ptrace_scope
    }
    let node = "/usr/local/bin/node";
    if !Path::new(node).exists() {
        return;
    }
    let f = fixture();
    let echo = echo_server(); // 127.0.0.1:<port> — an allowed upstream (127.0.0.0/8)
    let target = format!("127.0.0.1:{}", echo.port());
    let script = r#"
const net = require('net');
const u = new URL(process.env.HTTP_PROXY);
const pport = parseInt(u.port, 10);
const auth = 'Basic ' + Buffer.from(`${u.username}:`).toString('base64');
const s = net.connect(pport, '127.0.0.1');
let buf = '';
s.on('connect', () => s.write(`CONNECT ${process.argv[1]} HTTP/1.1\r\nHost: x\r\nProxy-Authorization: ${auth}\r\n\r\n`));
s.on('data', d => { buf += d; if (buf.includes('\r\n\r\n')) { s.destroy(); process.exit(buf.startsWith('HTTP/1.1 200') ? 0 : 5); } });
s.on('error', () => process.exit(3));
setTimeout(() => process.exit(7), 5000);
"#;
    let code = f.run(net_policy(), node, &["-e", script, &target]);
    assert_eq!(
        code, 0,
        "non-blocking node client must reach the proxy through the injected fd (got {code})"
    );
}
