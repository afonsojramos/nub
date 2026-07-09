//! Client-facing proxy handshakes: HTTP `CONNECT` and SOCKS5. Each parses the
//! requested target (host + port) WITHOUT yet committing to it — the caller runs the
//! target through the grant decider, then calls [`reply_success`]/[`reply_failure`].
//!
//! Why the two-step (parse, then caller replies): the client waits for the tunnel
//! ACK (`200` / SOCKS reply) before it sends its TLS ClientHello, so the proxy MUST
//! ACK before it can read the SNI. The target-host check happens BEFORE the ACK
//! (a denied host is refused outright); the SNI check happens AFTER (read the
//! ClientHello, then connect-or-drop). Both gates must pass.

use super::Host;
use std::io::{self, Read, Write};
use std::net::IpAddr;

/// Which client protocol a connection spoke — determines the reply framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Http,
    Socks5,
}

/// A parsed tunnel request: the protocol (for reply framing) and the requested target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub proto: Proto,
    pub host: Host,
    pub port: u16,
}

/// Max bytes we read while looking for the end of an HTTP CONNECT header block. A
/// real CONNECT request is short; past this it is junk → error (fail closed).
const MAX_HTTP_HEADER: usize = 8 * 1024;

/// Read + parse the client handshake, dispatching on the first byte (`0x05` = SOCKS5,
/// otherwise HTTP). Performs the SOCKS5 greeting round-trip (method selection) inline;
/// does NOT send the final tunnel reply (the caller does, post-decision).
pub fn read_request(stream: &mut (impl Read + Write)) -> io::Result<Request> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first)?;
    if first[0] == 0x05 {
        read_socks5(stream)
    } else {
        read_http_connect(stream, first[0])
    }
}

/// Send the tunnel-established ACK so the client begins its TLS handshake.
pub fn reply_success(stream: &mut impl Write, proto: Proto) -> io::Result<()> {
    match proto {
        Proto::Http => stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"),
        // VER=5 REP=0(success) RSV=0 ATYP=1(IPv4) BND.ADDR=0.0.0.0 BND.PORT=0
        Proto::Socks5 => stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
    }
}

/// Refuse the tunnel (denied target or SNI). The client sees a closed/refused tunnel.
pub fn reply_failure(stream: &mut impl Write, proto: Proto) -> io::Result<()> {
    match proto {
        Proto::Http => stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n"),
        // REP=2 = connection not allowed by ruleset.
        Proto::Socks5 => stream.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
    }
}

// ── SOCKS5 (RFC 1928) ────────────────────────────────────────────────────────────

/// The `0x05` version byte was already consumed by [`read_request`].
fn read_socks5(stream: &mut (impl Read + Write)) -> io::Result<Request> {
    // Greeting: NMETHODS(1) + METHODS(n). We accept no-auth unconditionally (the
    // proxy is loopback-only, reached solely by our own sandboxed child).
    let mut nmethods = [0u8; 1];
    stream.read_exact(&mut nmethods)?;
    let mut methods = vec![0u8; nmethods[0] as usize];
    stream.read_exact(&mut methods)?;
    stream.write_all(&[0x05, 0x00])?; // METHOD = no-auth

    // Request: VER(1) CMD(1) RSV(1) ATYP(1) …
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[0] != 0x05 {
        return Err(bad("socks5: bad request version"));
    }
    if head[1] != 0x01 {
        // Only CONNECT (0x01) is supported — no BIND/UDP-ASSOCIATE in a jail.
        return Err(bad("socks5: unsupported command"));
    }
    let host = match head[3] {
        0x01 => {
            let mut a = [0u8; 4];
            stream.read_exact(&mut a)?;
            Host::Ip(IpAddr::from(a))
        }
        0x04 => {
            let mut a = [0u8; 16];
            stream.read_exact(&mut a)?;
            Host::Ip(IpAddr::from(a))
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name)?;
            let name = String::from_utf8(name).map_err(|_| bad("socks5: non-utf8 host"))?;
            host_from_str(&name)
        }
        _ => return Err(bad("socks5: unknown address type")),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port)?;
    Ok(Request {
        proto: Proto::Socks5,
        host,
        port: u16::from_be_bytes(port),
    })
}

// ── HTTP CONNECT (RFC 7231 §4.3.6) ────────────────────────────────────────────────

/// `first` is the already-consumed first byte of the request line.
fn read_http_connect(stream: &mut (impl Read + Write), first: u8) -> io::Result<Request> {
    let mut buf = vec![first];
    // Read until the header terminator `\r\n\r\n`, bounded.
    let mut one = [0u8; 1];
    loop {
        if buf.len() >= MAX_HTTP_HEADER {
            return Err(bad("http connect: header too large"));
        }
        let n = stream.read(&mut one)?;
        if n == 0 {
            return Err(bad("http connect: eof before header end"));
        }
        buf.push(one[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    // First line: `CONNECT host:port HTTP/1.1`.
    let line_end = buf
        .windows(2)
        .position(|w| w == b"\r\n")
        .ok_or_else(|| bad("http connect: no request line"))?;
    let line = std::str::from_utf8(&buf[..line_end]).map_err(|_| bad("http connect: non-utf8"))?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(bad("http connect: not a CONNECT request"));
    }
    let authority = parts
        .next()
        .ok_or_else(|| bad("http connect: missing authority"))?;
    let (host, port) = split_authority(authority)?;
    Ok(Request {
        proto: Proto::Http,
        host,
        port,
    })
}

/// Split a `host:port` authority (with `[v6]:port` support) into host + port.
fn split_authority(authority: &str) -> io::Result<(Host, u16)> {
    let (host_str, port_str) = if let Some(rest) = authority.strip_prefix('[') {
        // [IPv6]:port
        let close = rest
            .find(']')
            .ok_or_else(|| bad("http connect: unterminated IPv6 authority"))?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = after
            .strip_prefix(':')
            .ok_or_else(|| bad("http connect: missing port"))?;
        (host, port)
    } else {
        let (h, p) = authority
            .rsplit_once(':')
            .ok_or_else(|| bad("http connect: missing port"))?;
        (h, p)
    };
    let port: u16 = port_str
        .parse()
        .map_err(|_| bad("http connect: bad port"))?;
    Ok((host_from_str(host_str), port))
}

/// Classify a host token as an IP literal or a name.
fn host_from_str(s: &str) -> Host {
    match s.parse::<IpAddr>() {
        Ok(ip) => Host::Ip(ip),
        Err(_) => Host::Name(s.to_string()),
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A duplex fake: `read` drains `input`, `write` appends to `output`.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Duplex {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn parses_http_connect_hostname() {
        let mut d = Duplex::new(b"CONNECT example.com:443 HTTP/1.1\r\nHost: x\r\n\r\n".to_vec());
        let r = read_request(&mut d).unwrap();
        assert_eq!(r.proto, Proto::Http);
        assert_eq!(r.host, Host::Name("example.com".into()));
        assert_eq!(r.port, 443);
    }

    #[test]
    fn parses_http_connect_ipv6() {
        let mut d = Duplex::new(b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n".to_vec());
        let r = read_request(&mut d).unwrap();
        assert_eq!(r.host, Host::Ip("::1".parse().unwrap()));
        assert_eq!(r.port, 8443);
    }

    #[test]
    fn parses_socks5_domain_request_and_selects_no_auth() {
        // greeting: ver5, 1 method (no-auth); request: connect, domain "a.example", :443
        let mut bytes = vec![0x05, 0x01, 0x00];
        bytes.extend_from_slice(&[0x05, 0x01, 0x00, 0x03]);
        let host = b"a.example";
        bytes.push(host.len() as u8);
        bytes.extend_from_slice(host);
        bytes.extend_from_slice(&443u16.to_be_bytes());
        let mut d = Duplex::new(bytes);
        let r = read_request(&mut d).unwrap();
        assert_eq!(r.proto, Proto::Socks5);
        assert_eq!(r.host, Host::Name("a.example".into()));
        assert_eq!(r.port, 443);
        // The method-selection reply (no-auth) must have been written.
        assert_eq!(&d.output, &[0x05, 0x00]);
    }

    #[test]
    fn parses_socks5_ipv4_request() {
        let mut bytes = vec![0x05, 0x01, 0x00];
        bytes.extend_from_slice(&[0x05, 0x01, 0x00, 0x01, 93, 184, 216, 34]);
        bytes.extend_from_slice(&443u16.to_be_bytes());
        let mut d = Duplex::new(bytes);
        let r = read_request(&mut d).unwrap();
        assert_eq!(r.host, Host::Ip("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn socks5_rejects_non_connect_command() {
        // cmd 0x02 = BIND — unsupported in a jail.
        let mut bytes = vec![0x05, 0x01, 0x00];
        bytes.extend_from_slice(&[0x05, 0x02, 0x00, 0x01, 1, 2, 3, 4, 0, 80]);
        let mut d = Duplex::new(bytes);
        assert!(read_request(&mut d).is_err());
    }

    #[test]
    fn http_non_connect_is_rejected() {
        let mut d = Duplex::new(b"GET http://x/ HTTP/1.1\r\n\r\n".to_vec());
        assert!(read_request(&mut d).is_err());
    }

    #[test]
    fn success_and_failure_replies_are_protocol_shaped() {
        let mut d = Duplex::new(vec![]);
        reply_success(&mut d, Proto::Http).unwrap();
        assert!(d.output.starts_with(b"HTTP/1.1 200"));
        let mut d = Duplex::new(vec![]);
        reply_failure(&mut d, Proto::Socks5).unwrap();
        assert_eq!(d.output[0..2], [0x05, 0x02]);
    }
}
