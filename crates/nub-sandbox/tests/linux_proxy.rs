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

fn landlock_available() -> bool {
    const SYS: libc::c_long = 444;
    let abi = unsafe { libc::syscall(SYS, std::ptr::null::<libc::c_void>(), 0usize, 1u64) };
    abi >= 2
}
fn landlock_net() -> bool {
    const SYS: libc::c_long = 444;
    let abi = unsafe { libc::syscall(SYS, std::ptr::null::<libc::c_void>(), 0usize, 1u64) };
    abi >= 4
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
// proxysni <tgt> <port> <sni> 0=forwarded 5=dropped 1=err. Reads HTTP_PROXY.
const PROBE_C: &str = r#"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

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
static int proxy_connect(const char* tgt,int tport){
    int pp=proxy_port(); if(!pp)return -1;
    int fd=dial("127.0.0.1",pp); if(fd<0)return -1;
    char req[256]; int n=snprintf(req,sizeof req,"CONNECT %s:%d HTTP/1.1\r\nHost: %s\r\n\r\n",tgt,tport,tgt);
    if(write(fd,req,n)!=n){close(fd);return -1;}
    char resp[512];int got=0;
    while(got<4||memcmp(resp+got-4,"\r\n\r\n",4)){int r=read(fd,resp+got,1);if(r<=0){close(fd);return -1;}got+=r;if(got>=(int)sizeof resp)break;}
    if(strncmp(resp,"HTTP/1.1 200",12)!=0){close(fd);return -2;}
    return fd;
}
int main(int argc,char**argv){
    if(argc<2)return 1;
    if(!strcmp(argv[1],"rawconnect")){int fd=dial(argv[2],atoi(argv[3]));if(fd<0)return 1;close(fd);return 0;}
    if(!strcmp(argv[1],"unixconnect")){
        int fd=socket(AF_UNIX,SOCK_STREAM,0); if(fd<0)return 1; /* socket() denied */
        struct sockaddr_un a; memset(&a,0,sizeof a); a.sun_family=AF_UNIX;
        strncpy(a.sun_path,argv[2],sizeof a.sun_path-1);
        if(connect(fd,(struct sockaddr*)&a,sizeof a)!=0){close(fd);return 1;} close(fd);return 0;
    }
    if(!strcmp(argv[1],"proxysni")){
        int fd=proxy_connect(argv[2],atoi(argv[3])); if(fd<0)return 1;
        unsigned char hello[1024];int hl=build_hello(argv[4],hello);
        if(write(fd,hello,hl)!=hl){close(fd);return 1;}
        unsigned char e[64]; int r=read(fd,e,sizeof e); close(fd); return (r>0)?0:5;
    }
    return 1;
}
"#;

fn net_policy() -> Value {
    json!({ "fs": true, "net": ["127.0.0.0/8", "*.allowed.example"] })
}

#[test]
fn per_host_proxy_forwards_allowed_drops_denied_blocks_direct() {
    if !landlock_net() {
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
    // Negative control: net relaxed → the direct connect succeeds.
    assert_eq!(
        f.run(
            json!({ "fs": true }),
            probe,
            &["rawconnect", &echo.ip().to_string(), &port]
        ),
        0,
        "neg-control: unenforced net connects directly"
    );
}

#[test]
fn af_unix_egress_closed_under_net_deny() {
    if !landlock_available() {
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
    // (docker.sock class) is closed.
    assert_eq!(
        f.run(json!({ "net": false }), probe, &["unixconnect", sock]),
        1,
        "AF_UNIX connect to docker.sock must be denied under net-deny"
    );
    // Negative control: no net enforcement → the same AF_UNIX connect succeeds.
    assert_eq!(
        f.run(json!({ "fs": true }), probe, &["unixconnect", sock]),
        0,
        "neg-control: unenforced net reaches the unix socket"
    );
}

#[test]
fn node_fork_ipc_survives_net_deny() {
    // The decisive empirical check: denying socket(AF_UNIX) must NOT break node's fork
    // IPC, which rides socketpair(AF_UNIX) (kept allowed). Under net:false the outer
    // node spawns a child over an IPC channel and exchanges a message.
    if !landlock_available() {
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
