//! Egress proxy — host-runnable integration tests (no OS sandbox needed).
//!
//! Each test starts a real [`EgressProxy`] on a loopback port, drives a real HTTP
//! CONNECT or SOCKS5 client through it, and asserts allowed tunnels forward while
//! denied ones drop — including the SNI gate (a denied SNI to an admitted target IP
//! is dropped). Upstreams are throwaway loopback echo servers, so the whole matrix is
//! hermetic; no external host is contacted. The "ClientHello" is a well-formed SNI
//! byte blob (the proxy does NOT terminate TLS, so the echo server just reflects it).

use nub_sandbox::StaticDecider;
use nub_sandbox::policy::{Effect, NetPolicy, NetRule, NetTarget};
use nub_sandbox::proxy::{Decision, EgressProxy, GrantDecider, Host};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── throwaway upstream: a loopback echo server ──────────────────────────────────

/// Start a loopback echo server that reflects bytes on each connection until EOF.
/// Returns its address; the accept thread is detached (dies with the test process).
fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

// ── a well-formed ClientHello carrying a chosen SNI ─────────────────────────────

fn client_hello(sni: &str) -> Vec<u8> {
    let host = sni.as_bytes();
    let mut sn = vec![0x00]; // name_type host_name
    sn.extend_from_slice(&(host.len() as u16).to_be_bytes());
    sn.extend_from_slice(host);
    let mut list = Vec::new();
    list.extend_from_slice(&(sn.len() as u16).to_be_bytes());
    list.extend_from_slice(&sn);
    let mut exts = Vec::new();
    exts.extend_from_slice(&0x0000u16.to_be_bytes()); // server_name ext
    exts.extend_from_slice(&(list.len() as u16).to_be_bytes());
    exts.extend_from_slice(&list);

    let mut body = Vec::new();
    body.extend_from_slice(&[0x03, 0x03]); // version
    body.extend_from_slice(&[0u8; 32]); // random
    body.push(0); // session id
    body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // cipher suites
    body.extend_from_slice(&[0x01, 0x00]); // compression
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    body.extend_from_slice(&exts);

    let mut hs = vec![0x01]; // ClientHello
    let l = body.len();
    hs.extend_from_slice(&[(l >> 16) as u8, (l >> 8) as u8, l as u8]);
    hs.extend_from_slice(&body);

    // one TLS record
    let mut rec = vec![0x16, 0x03, 0x01];
    rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    rec.extend_from_slice(&hs);
    rec
}

// ── proxy client helpers ────────────────────────────────────────────────────────

/// HTTP CONNECT to `target` (a `host:port` authority) through the proxy. Returns the
/// tunnel stream after the `200` ACK, or an error string on a non-2xx response.
fn http_connect(proxy_port: u16, target: &str) -> Result<TcpStream, String> {
    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(s, "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").unwrap();
    let mut resp = Vec::new();
    let mut one = [0u8; 1];
    loop {
        match s.read(&mut one) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                resp.push(one[0]);
                if resp.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let head = String::from_utf8_lossy(&resp);
    if head.starts_with("HTTP/1.1 200") {
        Ok(s)
    } else {
        Err(head.lines().next().unwrap_or("").to_string())
    }
}

/// SOCKS5 CONNECT to an IPv4 `addr` through the proxy. Returns the tunnel stream after
/// a success reply, or `Err` on a non-success reply.
fn socks5_connect_ip(proxy_port: u16, addr: SocketAddr) -> Result<TcpStream, u8> {
    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(&[0x05, 0x01, 0x00]).unwrap(); // greeting: 1 method (no-auth)
    let mut sel = [0u8; 2];
    s.read_exact(&mut sel).unwrap();
    assert_eq!(sel, [0x05, 0x00]);
    let ip = match addr.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => panic!("ipv4 only"),
    };
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&ip);
    req.extend_from_slice(&addr.port().to_be_bytes());
    s.write_all(&req).unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).unwrap();
    if rep[1] == 0x00 { Ok(s) } else { Err(rep[1]) }
}

/// Send a ClientHello with `sni` over an established tunnel and report whether the
/// upstream echo reflected it (i.e. the tunnel forwarded, not dropped).
fn tunnel_forwards(stream: &mut TcpStream, sni: &str) -> bool {
    let hello = client_hello(sni);
    if stream.write_all(&hello).is_err() {
        return false;
    }
    let mut got = vec![0u8; hello.len()];
    read_full(stream, &mut got)
        .map(|()| got == hello)
        .unwrap_or(false)
}

/// Read exactly `buf.len()` bytes or fail (EOF/timeout on a dropped tunnel → Err).
fn read_full(stream: &mut TcpStream, buf: &mut [u8]) -> Result<(), ()> {
    let mut off = 0;
    while off < buf.len() {
        match stream.read(&mut buf[off..]) {
            Ok(0) | Err(_) => return Err(()),
            Ok(n) => off += n,
        }
    }
    Ok(())
}

// ── policy helpers ──────────────────────────────────────────────────────────────

fn net(rules: Vec<NetRule>) -> NetPolicy {
    NetPolicy {
        enforce: true,
        rules,
        default_effect: Effect::Deny,
    }
}
fn allow_host(pat: &str) -> NetRule {
    NetRule {
        target: NetTarget::Host(pat.to_string()),
        effect: Effect::Allow,
    }
}
fn allow_cidr(cidr: &str) -> NetRule {
    NetRule {
        target: NetTarget::Cidr(cidr.parse().unwrap()),
        effect: Effect::Allow,
    }
}

fn start(policy: NetPolicy) -> EgressProxy {
    EgressProxy::start(Arc::new(StaticDecider::new(policy))).unwrap()
}

// ── tests ────────────────────────────────────────────────────────────────────────

#[test]
fn http_connect_allowed_host_forwards() {
    let upstream = echo_server();
    // Allow the loopback CIDR (gate 1: target IP) AND the SNI host glob (gate 2).
    let proxy = start(net(vec![
        allow_cidr("127.0.0.0/8"),
        allow_host("*.allowed.example"),
    ]));
    let mut t = http_connect(proxy.port(), &format!("127.0.0.1:{}", upstream.port())).unwrap();
    assert!(
        tunnel_forwards(&mut t, "api.allowed.example"),
        "an allowed SNI to an admitted target must forward end-to-end"
    );
}

#[test]
fn http_connect_denied_sni_drops() {
    let upstream = echo_server();
    // Target IP admitted (gate 1), but the SNI is NOT on the allow-list (gate 2).
    let proxy = start(net(vec![
        allow_cidr("127.0.0.0/8"),
        allow_host("*.allowed.example"),
    ]));
    let mut t = http_connect(proxy.port(), &format!("127.0.0.1:{}", upstream.port())).unwrap();
    assert!(
        !tunnel_forwards(&mut t, "evil.example"),
        "a denied SNI must be dropped even when the target IP is admitted (shared-IP guard)"
    );
}

#[test]
fn http_connect_denied_target_host_refused_before_ack() {
    let upstream = echo_server();
    // Only a host glob is allowed; the loopback IP target is NOT admitted → gate 1
    // refuses with a non-200 before any tunnel is established.
    let proxy = start(net(vec![allow_host("*.allowed.example")]));
    let err = http_connect(proxy.port(), &format!("127.0.0.1:{}", upstream.port())).unwrap_err();
    assert!(
        err.contains("403"),
        "denied target must get a 403, got {err:?}"
    );
}

#[test]
fn http_connect_hostname_target_resolves_and_forwards() {
    // The hostname path: `localhost` is allowed + resolves to loopback; the proxy owns
    // DNS. SNI `localhost` also admitted.
    let upstream = echo_server();
    let proxy = start(net(vec![allow_host("localhost")]));
    let mut t = http_connect(proxy.port(), &format!("localhost:{}", upstream.port())).unwrap();
    assert!(
        tunnel_forwards(&mut t, "localhost"),
        "an allowed hostname target must resolve and forward"
    );
}

#[test]
fn socks5_allowed_forwards_denied_sni_drops() {
    let upstream = echo_server();
    let proxy = start(net(vec![
        allow_cidr("127.0.0.0/8"),
        allow_host("*.allowed.example"),
    ]));
    // allowed SNI over SOCKS5
    let mut ok = socks5_connect_ip(proxy.port(), upstream).unwrap();
    assert!(
        tunnel_forwards(&mut ok, "cdn.allowed.example"),
        "socks5 allow forwards"
    );
    // denied SNI over SOCKS5 → dropped
    let mut bad = socks5_connect_ip(proxy.port(), upstream).unwrap();
    assert!(
        !tunnel_forwards(&mut bad, "evil.example"),
        "socks5 denied SNI drops"
    );
}

#[test]
fn socks5_denied_target_ip_gets_refusal_reply() {
    let upstream = echo_server();
    // No CIDR allowed → the loopback target IP is refused at the SOCKS request reply.
    let proxy = start(net(vec![allow_host("*.allowed.example")]));
    let rep = socks5_connect_ip(proxy.port(), upstream).unwrap_err();
    assert_eq!(rep, 0x02, "SOCKS5 refusal REP=2 (not allowed by ruleset)");
}

#[test]
fn non_tls_stream_to_admitted_target_forwards() {
    // A non-TLS payload (first byte != 0x16) to an admitted target has no SNI to
    // cross-route on → forwarded. Proves NotTls admits (not fail-closed).
    let upstream = echo_server();
    let proxy = start(net(vec![allow_cidr("127.0.0.0/8")]));
    let mut t = http_connect(proxy.port(), &format!("127.0.0.1:{}", upstream.port())).unwrap();
    let payload = b"PING plain-tcp\n";
    t.write_all(payload).unwrap();
    let mut got = vec![0u8; payload.len()];
    assert!(read_full(&mut t, &mut got).is_ok() && got == payload);
}

#[test]
fn stalled_tls_tunnel_fails_closed() {
    // Client ACKs then sends the START of a TLS record but never completes the
    // ClientHello (a partial handshake). The proxy must NOT splice — it fails closed
    // rather than let a later denied-SNI cross-route. We assert the tunnel is dropped
    // (the read side closes without echoing our partial bytes).
    let _upstream = echo_server();
    let proxy = start(net(vec![
        allow_cidr("127.0.0.0/8"),
        allow_host("*.allowed.example"),
    ]));
    let mut t = http_connect(proxy.port(), &format!("127.0.0.1:{}", _upstream.port())).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    // A handshake record header claiming a large body, then nothing more.
    t.write_all(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00])
        .unwrap();
    let mut got = [0u8; 16];
    // With no complete ClientHello, the proxy waits (up to its own timeout) then
    // drops — the client read returns 0/err, never an echo of our bytes.
    let dropped = matches!(t.read(&mut got), Ok(0) | Err(_));
    assert!(
        dropped,
        "a stalled TLS tunnel must fail closed (be dropped)"
    );
}

#[test]
fn decider_seam_is_consulted_for_target_and_sni() {
    // A recording decider proves BOTH gates fire: the CONNECT target AND the SNI are
    // each passed to the callback seam (the interactive-prompt swap point).
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<String>>,
    }
    impl GrantDecider for Recorder {
        fn decide(&self, host: &Host) -> Decision {
            let key = match host {
                Host::Name(n) => n.clone(),
                Host::Ip(ip) => ip.to_string(),
            };
            self.seen.lock().unwrap().push(key.clone());
            // Allow the loopback target + the allowed SNI; deny everything else.
            if key == "127.0.0.1" || key == "keep.allowed.example" {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
    }
    let upstream = echo_server();
    let rec = Arc::new(Recorder::default());
    let proxy = EgressProxy::start(rec.clone()).unwrap();
    let mut t = http_connect(proxy.port(), &format!("127.0.0.1:{}", upstream.port())).unwrap();
    assert!(tunnel_forwards(&mut t, "keep.allowed.example"));
    let seen = rec.seen.lock().unwrap().clone();
    assert!(
        seen.contains(&"127.0.0.1".to_string()),
        "target host consulted"
    );
    assert!(
        seen.contains(&"keep.allowed.example".to_string()),
        "SNI consulted via the same seam"
    );
}

#[test]
fn dropping_proxy_stops_the_listener() {
    let proxy = start(net(vec![allow_cidr("127.0.0.0/8")]));
    let port = proxy.port();
    // Reachable while alive.
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
    drop(proxy);
    // After drop the listener is closed; a connect now fails (give the accept thread a
    // moment to unwind).
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the proxy port must be closed after the handle drops"
    );
}
