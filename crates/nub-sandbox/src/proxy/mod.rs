//! The localhost egress proxy (design.md §2.5): the per-host policy engine for the
//! net axis. NO MITM.
//!
//! MECHANISM. The OS deny-layer (each backend) forces the sandboxed child's egress
//! to reach ONLY this proxy on loopback; direct external egress is blocked at the
//! kernel. The proxy speaks HTTP `CONNECT` and SOCKS5, and enforces per-host policy
//! in TWO gates, both of which must pass: (1) the CONNECT/SOCKS **target host**
//! (checked before the tunnel ACK), and (2) the TLS **SNI** read in the clear from
//! the client's ClientHello (checked after the ACK, before connecting upstream) —
//! [`sni`], no key, no CA. An allowed tunnel is blind-forwarded byte-for-byte; a
//! denied one is dropped before the upstream socket is ever opened.
//!
//! FAIL-CLOSED. The decision is a [`GrantDecider`] seam (`Fn(&Host) -> Decision`) —
//! wired to the STATIC policy here ([`StaticDecider`]); the build-jail thread later
//! swaps in an interactive prompt without touching this file. A TLS tunnel whose
//! ClientHello is malformed, or stalls without a checkable SNI, is DENIED — a
//! stall-then-send-denied-SNI cannot slip past (see [`read_and_check_sni`]).
//!
//! LIFECYCLE. Thread-per-connection over `std::net` — NO async runtime, NO new
//! dependency. The proxy runs in the nub PARENT process and outlives the child:
//! [`apply`](crate::apply) stashes the [`EgressProxy`] in [`Prepared`](crate::Prepared)
//! so it lives for the child's whole run and shuts down when that value drops.

mod handshake;
mod sni;

use crate::matcher::HostMatcher;
use crate::policy::NetPolicy;
use handshake::{read_request, reply_failure, reply_success};
use sni::SniScan;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// A host the proxy makes an egress decision about. The seam type of [`GrantDecider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// A hostname (from a CONNECT authority, a SOCKS5 domain, or a TLS SNI).
    Name(String),
    /// An IP literal (a SOCKS5 IPv4/IPv6 target or an IP-form CONNECT authority).
    Ip(IpAddr),
}

/// A grant decision for one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// The egress grant seam. The proxy consults it for the CONNECT/SOCKS target AND for
/// the TLS SNI; both must be [`Decision::Allow`]. This epic wires it to the static
/// policy ([`StaticDecider`]); the build-jail thread swaps in an interactive prompt.
pub trait GrantDecider: Send + Sync + 'static {
    fn decide(&self, host: &Host) -> Decision;
}

/// The static-policy decider: evaluates a resolved [`NetPolicy`] last-match-wins via
/// the shared [`HostMatcher`], so the proxy's per-host verdict is byte-identical to
/// the IR's net matcher (one source of truth for host-glob + CIDR semantics).
pub struct StaticDecider {
    policy: NetPolicy,
}

impl StaticDecider {
    pub fn new(policy: NetPolicy) -> Self {
        Self { policy }
    }
}

impl GrantDecider for StaticDecider {
    fn decide(&self, host: &Host) -> Decision {
        let key = match host {
            Host::Name(n) => n.clone(),
            Host::Ip(ip) => ip.to_string(),
        };
        if HostMatcher::new(&self.policy).admits(&key) {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

/// Time budget for reading the client's first bytes (the TLS ClientHello) after the
/// tunnel ACK. A legit HTTPS client sends it immediately; a client that stalls past
/// this is denied (the stall-bypass guard).
const CLIENT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Connect timeout to the upstream target.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap on buffered client prelude bytes while scanning for the SNI (mirrors the SNI
/// reassembly cap): past this a client is dribbling → fail closed.
const MAX_PRELUDE: usize = 16 * 1024;

/// A running egress proxy bound to `127.0.0.1:<port>`. Dropping it stops accepting new
/// connections (the parent owns this; it drops after the sandboxed child exits).
pub struct EgressProxy {
    port: u16,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl EgressProxy {
    /// Bind a loopback listener and start the accept loop. `decider` gates every
    /// tunnel. Returns once the port is bound (so a caller can wire the port into the
    /// backend deny-layer before spawning the child).
    pub fn start(decider: Arc<dyn GrantDecider>) -> io::Result<EgressProxy> {
        // Loopback only — the sandboxed child reaches us via 127.0.0.1; nothing off-box
        // should ever see this listener.
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0))?;
        let port = listener.local_addr()?.port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let sh = shutdown.clone();
        let accept_thread = std::thread::Builder::new()
            .name("nub-egress-proxy".into())
            .spawn(move || accept_loop(listener, sh, decider))?;
        Ok(EgressProxy {
            port,
            shutdown,
            accept_thread: Some(accept_thread),
        })
    }

    /// The loopback port the child must be pointed at (env hint + OS carve-out).
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        // Signal the accept loop, then wake its blocked `accept()` with a throwaway
        // self-connection so it observes the flag and exits. In-flight tunnel threads
        // are detached; they end when their sockets close (the child is already gone).
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((IpAddr::from([127, 0, 0, 1]), self.port));
        if let Some(h) = self.accept_thread.take() {
            let _ = h.join();
        }
    }
}

/// Accept loop: one detached handler thread per connection. Any handler error just
/// closes that connection — a single malformed client never takes down the proxy.
fn accept_loop(listener: TcpListener, shutdown: Arc<AtomicBool>, decider: Arc<dyn GrantDecider>) {
    for conn in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        let d = decider.clone();
        // Best-effort spawn; if the OS refuses a thread we simply drop the connection
        // (fail-closed — no unproxied path opens).
        let _ = std::thread::Builder::new()
            .name("nub-egress-tunnel".into())
            .spawn(move || {
                let _ = handle_conn(stream, d);
            });
    }
}

/// Handle one client tunnel: parse the request, gate the target host, ACK, gate the
/// SNI, then connect upstream and blind-forward. Returns `Ok(())` on any clean refusal.
fn handle_conn(mut stream: TcpStream, decider: Arc<dyn GrantDecider>) -> io::Result<()> {
    stream.set_read_timeout(Some(CLIENT_HELLO_TIMEOUT))?;
    let req = read_request(&mut stream)?;

    // Gate 1 — the CONNECT/SOCKS target host (before the ACK).
    if decider.decide(&req.host) == Decision::Deny {
        let _ = reply_failure(&mut stream, req.proto);
        return Ok(());
    }
    reply_success(&mut stream, req.proto)?;

    // Gate 2 — the TLS SNI, read no-MITM from the client's first bytes.
    let (prelude, allowed) = read_and_check_sni(&mut stream, decider.as_ref())?;
    if !allowed {
        return Ok(()); // drop — the client sees a reset tunnel
    }

    // Connect upstream ONLY after both gates pass, replay the buffered prelude, splice.
    let upstream = connect_upstream(&req.host, req.port)?;
    stream.set_read_timeout(None)?;
    upstream.set_read_timeout(None)?;
    let mut up = upstream;
    if !prelude.is_empty() {
        up.write_all(&prelude)?;
    }
    splice(stream, up);
    Ok(())
}

/// Read the client's first bytes and decide the SNI gate. Returns the buffered prelude
/// (to replay upstream) and whether the tunnel is allowed.
///
/// The rule closes the SNI-evasion vectors: a complete ClientHello's SNI is checked;
/// a ClientHello with no SNI, or a non-TLS stream, admits (the target host already
/// passed gate 1, and without an SNI a shared-IP host cannot cross-route); a TLS
/// ClientHello that is malformed, oversize, or stalls without completing (incl. the
/// client ACKing then sending nothing) FAILS CLOSED — so a "send a partial hello, then
/// send a denied SNI after we splice" attack cannot bypass gate 2.
fn read_and_check_sni(
    stream: &mut TcpStream,
    decider: &dyn GrantDecider,
) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => return Ok(finalize_scan(&buf, decider)),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                match sni::scan_client_hello(&buf) {
                    SniScan::Sni(host) => {
                        let ok = decider.decide(&Host::Name(host)) == Decision::Allow;
                        return Ok((buf, ok));
                    }
                    // Admitted target + no SNI to cross-route on → allow.
                    SniScan::NoSni | SniScan::NotTls => return Ok((buf, true)),
                    // TLS-shaped but broken → fail closed.
                    SniScan::Malformed => return Ok((buf, false)),
                    SniScan::Incomplete => {
                        if buf.len() > MAX_PRELUDE {
                            return Ok((buf, false)); // dribbling past the cap → fail closed
                        }
                        // else read more
                    }
                }
            }
            Err(e) if is_timeout(&e) => return Ok(finalize_scan(&buf, decider)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Decide on whatever prelude arrived when the read ends (EOF or timeout). A complete
/// hello is honored; an incomplete/empty TLS stream (the stall) fails closed.
fn finalize_scan(buf: &[u8], decider: &dyn GrantDecider) -> (Vec<u8>, bool) {
    match sni::scan_client_hello(buf) {
        SniScan::Sni(host) => {
            let ok = decider.decide(&Host::Name(host.clone())) == Decision::Allow;
            (buf.to_vec(), ok)
        }
        SniScan::NoSni | SniScan::NotTls => (buf.to_vec(), true),
        // Incomplete (incl. an empty buffer — client ACK'd then sent nothing) or
        // Malformed → the SNI could not be verified → deny.
        SniScan::Incomplete | SniScan::Malformed => (buf.to_vec(), false),
    }
}

/// Egress addresses the proxy must NEVER connect to, even when policy admits the host.
///
/// SSRF / DNS-rebinding guard. An allowed hostname that resolves — or an attacker's DNS
/// rebinds — to the cloud-metadata / link-local surface is refused at the connect. It
/// covers IPv4 link-local `169.254.0.0/16` (incl. the `169.254.169.254` IMDS endpoint),
/// IPv6 link-local `fe80::/10`, and the AWS IPv6 IMDS `fd00:ec2::254`; an IPv4-in-IPv6
/// form (`::ffff:169.254.169.254`, `::169.254.169.254`) is unmapped to its embedded v4
/// FIRST so the encoding can't smuggle a metadata address past as an IPv6 literal. All
/// integer/octal/hex host encodings are already normalized away here because we classify
/// the RESOLVED [`IpAddr`], not the child-supplied token. Loopback is deliberately NOT
/// blocked (the proxy's own carve is loopback, and a legit upstream may be); broad RFC1918
/// private-range blocking is a separate maintainer posture call (see LIMITATIONS.md).
fn is_blocked_egress_ip(ip: IpAddr) -> bool {
    const AWS_IMDS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return v4.is_link_local();
            }
            // fe80::/10 hand-rolled (`Ipv6Addr::is_unicast_link_local` is still unstable).
            (v6.segments()[0] & 0xffc0) == 0xfe80 || v6 == AWS_IMDS_V6
        }
    }
}

/// Connect to the upstream target with a timeout. A hostname is resolved here (the
/// proxy owns DNS — a child-supplied IP for a hostname is never trusted).
///
/// ANTI-REBINDING PIN: the name is resolved exactly ONCE into a fixed address list, and
/// each address is SSRF-classified and connected to as the SAME `SocketAddr` — there is
/// no second resolution between the check and the connect, so DNS cannot swap in a
/// metadata IP after validation. A resolved address on the blocked surface is skipped
/// (fail-closed); a host that resolves ONLY to blocked addresses yields the block error.
fn connect_upstream(host: &Host, port: u16) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = match host {
        Host::Ip(ip) => vec![SocketAddr::new(*ip, port)],
        Host::Name(name) => (name.as_str(), port).to_socket_addrs()?.collect(),
    };
    let mut last_err = io::Error::other("no address resolved");
    for addr in addrs {
        if is_blocked_egress_ip(addr.ip()) {
            last_err = io::Error::new(
                io::ErrorKind::PermissionDenied,
                "egress to a link-local/metadata address is blocked",
            );
            continue;
        }
        match TcpStream::connect_timeout(&addr, UPSTREAM_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Blind bidirectional forward. One thread copies client→upstream; this thread copies
/// upstream→client. Each direction shuts down the peer's write half on EOF so the
/// other copy unblocks and the tunnel tears down cleanly.
fn splice(client: TcpStream, upstream: TcpStream) {
    let Ok(mut client_rd) = client.try_clone() else {
        return;
    };
    let Ok(mut up_wr) = upstream.try_clone() else {
        return;
    };
    let c2u = std::thread::spawn(move || {
        let _ = io::copy(&mut client_rd, &mut up_wr);
        let _ = up_wr.shutdown(Shutdown::Write);
    });
    let mut up_rd = upstream;
    let mut client_wr = client;
    let _ = io::copy(&mut up_rd, &mut client_wr);
    let _ = client_wr.shutdown(Shutdown::Write);
    let _ = c2u.join();
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Effect, NetRule, NetTarget};

    fn net(rules: Vec<NetRule>, default_effect: Effect) -> NetPolicy {
        NetPolicy {
            enforce: true,
            rules,
            default_effect,
        }
    }
    fn host(pat: &str, effect: Effect) -> NetRule {
        NetRule {
            target: NetTarget::Host(pat.to_string()),
            effect,
        }
    }

    #[test]
    fn static_decider_matches_host_glob_and_cidr() {
        let policy = net(
            vec![
                host("*.allowed.example", Effect::Allow),
                NetRule {
                    target: NetTarget::Cidr("10.0.0.0/8".parse().unwrap()),
                    effect: Effect::Allow,
                },
            ],
            Effect::Deny,
        );
        let d = StaticDecider::new(policy);
        assert_eq!(
            d.decide(&Host::Name("api.allowed.example".into())),
            Decision::Allow
        );
        assert_eq!(
            d.decide(&Host::Name("allowed.example".into())),
            Decision::Allow // apex matches *.allowed.example
        );
        assert_eq!(d.decide(&Host::Name("evil.example".into())), Decision::Deny);
        assert_eq!(
            d.decide(&Host::Ip("10.1.2.3".parse().unwrap())),
            Decision::Allow
        );
        assert_eq!(
            d.decide(&Host::Ip("8.8.8.8".parse().unwrap())),
            Decision::Deny
        );
    }

    #[test]
    fn blocks_metadata_and_link_local_egress() {
        let blocked = [
            "169.254.169.254",        // AWS/GCP/Azure IMDS (IPv4 link-local)
            "169.254.0.1",            // link-local edge
            "fe80::1",                // IPv6 link-local
            "fe80::a9fe:a9fe",        // IPv6 link-local, arbitrary suffix
            "febf::1",                // fe80::/10 upper edge
            "fd00:ec2::254",          // AWS IPv6 IMDS
            "::ffff:169.254.169.254", // IPv4-mapped metadata (encoding smuggle)
            "::169.254.169.254",      // IPv4-compat metadata (encoding smuggle)
        ];
        for ip in blocked {
            assert!(
                is_blocked_egress_ip(ip.parse().unwrap()),
                "{ip} must be classified as blocked egress"
            );
        }
        // NOT blocked: loopback (the proxy carve + loopback upstreams), public, and —
        // deliberately, pending the maintainer posture call — RFC1918 private ranges.
        let allowed = [
            "127.0.0.1",
            "::1",
            "8.8.8.8",
            "203.0.113.10",
            "2606:4700:4700::1111",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1", // RFC1918: NOT blocked in this change
        ];
        for ip in allowed {
            assert!(
                !is_blocked_egress_ip(ip.parse().unwrap()),
                "{ip} must NOT be classified as blocked egress"
            );
        }
    }

    #[test]
    fn connect_upstream_denies_link_local_but_reaches_allowed_target() {
        // Negative control: an allowed (non-blocked) target actually connects.
        let echo = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _ = echo.accept();
        });
        assert!(
            connect_upstream(&Host::Ip(IpAddr::from([127, 0, 0, 1])), port).is_ok(),
            "an allowed target must still connect through the guard"
        );

        // The guard denies a metadata target immediately (PermissionDenied), without
        // attempting the connect — so a live metadata endpoint would never be reached.
        let err = connect_upstream(&Host::Ip("169.254.169.254".parse().unwrap()), 80)
            .expect_err("link-local egress must be blocked");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn static_decider_last_match_wins() {
        // `["*", "!*.evil.example"]`: allow-all then deny a subtree.
        let policy = net(
            vec![
                host("*", Effect::Allow),
                host("*.evil.example", Effect::Deny),
            ],
            Effect::Deny,
        );
        let d = StaticDecider::new(policy);
        assert_eq!(d.decide(&Host::Name("ok.example".into())), Decision::Allow);
        assert_eq!(
            d.decide(&Host::Name("x.evil.example".into())),
            Decision::Deny
        );
    }
}
