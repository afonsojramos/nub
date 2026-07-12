//! Linux credential-brokering (U5 MITM tier) — REAL enforcement e2e (Landlock v4 +
//! seccomp connect-notify + the live egress proxy WITH TLS termination). Real-kernel
//! only; a no-Landlock kernel skips (or PANICS under `NUB_SANDBOX_REQUIRE_LANDLOCK`).
//!
//! The component tiers below MITM are proven elsewhere (host-runnable `proxy.rs`, the
//! `linux_proxy.rs` net axis, `mitm.rs`/`ca.rs`/`compiler.rs` units). What was UNPROVEN
//! on Linux until here: the FULL brokering flow driving a REAL sandboxed `node` through
//! the terminating proxy end-to-end. This suite closes that gap, each assertion paired
//! with the security invariant it defends and made non-vacuous by a differential:
//!
//!   - server-side INJECTION: the upstream receives the REAL secret, the child's own
//!     `Authorization` is STRIPPED (never forwarded) — the strip-then-set contract;
//!   - the child NEVER holds the secret in its (scrubbed) environment;
//!   - trust reaches the child ONLY via the ephemeral, temp `NODE_EXTRA_CA_CERTS` bundle
//!     (the child TLS-verifies the minted leaf), and that bundle is GONE after the run;
//!   - FAIL-CLOSED: an unverified upstream (rogue cert) yields NO forwarded request —
//!     the secret never crosses an unverified channel — and the child gets no response;
//!   - a wildcard broker is rejected (the credential-laundering guard) at compile.
//!
//! THE UPSTREAM-VERIFICATION SEAM. The proxy's outbound leg verifies the real server
//! against the platform roots (`rustls_native_certs::load_native_certs`, which honors
//! `SSL_CERT_FILE`). A hermetic local upstream is therefore made verifiable by pointing
//! `SSL_CERT_FILE` at a bundle holding a throwaway test CA — process-scoped, the OS trust
//! store is never touched. The brokered host is `localhost` (a literal — wildcards are
//! rejected) so the SNI the child sends resolves loopback to the in-process test server.
#![cfg(target_os = "linux")]

use nub_sandbox::compiler::{CompileCtx, ShellRunner};
use nub_sandbox::matcher::Homes;
use nub_sandbox::{CommandSpec, apply, compile};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ── real-kernel gating (mirrors linux_proxy.rs) ─────────────────────────────────

fn landlock_abi() -> libc::c_long {
    const SYS: libc::c_long = 444;
    let abi = unsafe { libc::syscall(SYS, std::ptr::null::<libc::c_void>(), 0usize, 1u64) };
    abi.max(0)
}

fn require_landlock() -> bool {
    matches!(
        std::env::var("NUB_SANDBOX_REQUIRE_LANDLOCK").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// SKIP when Landlock ABI is below `min` — unless the conformance leg demands it
/// (`NUB_SANDBOX_REQUIRE_LANDLOCK`), where a missing capability PANICS so a hollow skip
/// can't read as green.
fn skip_without_landlock(min: libc::c_long) -> bool {
    if landlock_abi() >= min {
        return false;
    }
    assert!(
        !require_landlock(),
        "NUB_SANDBOX_REQUIRE_LANDLOCK set but Landlock ABI < {min} — MITM brokering \
         cannot be proven on this kernel (real-kernel gate)"
    );
    true
}

fn ptrace_scope() -> i32 {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

const NODE: &str = "/usr/local/bin/node";

/// The distinctive secret the broker injects. Only the value's `needle` substring is ever
/// handed to the child (as the search term for its env-scan) — never the secret itself.
const SECRET: &str = "Bearer nub-mitm-e2e-REAL-SECRET-do-not-leak-7f3a";
const NEEDLE: &str = "nub-mitm-e2e-REAL-SECRET";
/// The child's own Authorization — a decoy that strip-then-set must remove upstream.
const CHILD_DECOY: &str = "Bearer child-decoy-must-be-stripped";

// ── the trust seam ──────────────────────────────────────────────────────────────
//
// One throwaway CA anchors the WHOLE process: its cert is written to the `SSL_CERT_FILE`
// bundle (so the proxy's upstream leg trusts a leaf it signs), and it signs the trusted
// upstream's `localhost` leaf. All engine builds in this process observe the same seam,
// so tests are SERIALIZED (`SEAM`) and the env is set once, before any `apply()`.

/// A minimal throwaway CA (rcgen), able to mint `localhost` server leaves.
struct TestCa {
    cert: Certificate,
    key: KeyPair,
    pem: String,
}

impl TestCa {
    fn generate(cn: &str) -> TestCa {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.distinguished_name.push(DnType::CommonName, cn);
        let cert = params.self_signed(&key).unwrap();
        let pem = cert.pem();
        TestCa { cert, key, pem }
    }

    fn leaf_for(&self, host: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let leaf_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![host.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.distinguished_name.push(DnType::CommonName, host);
        let leaf = params.signed_by(&leaf_key, &self.cert, &self.key).unwrap();
        let chain = vec![leaf.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        (chain, key)
    }
}

static TRUSTED_CA: LazyLock<TestCa> =
    LazyLock::new(|| TestCa::generate("nub e2e trusted upstream CA"));
static SEAM: Mutex<()> = Mutex::new(());

/// The `SSL_CERT_FILE` bundle file — kept alive for the process so the path stays valid.
static SSL_CERT_FILE: LazyLock<tempfile::NamedTempFile> = LazyLock::new(|| {
    let mut f = tempfile::Builder::new()
        .prefix("nub-e2e-roots-")
        .suffix(".pem")
        .tempfile()
        .unwrap();
    f.write_all(TRUSTED_CA.pem.as_bytes()).unwrap();
    f.flush().unwrap();
    // SAFETY: set ONCE, before any `apply()` builds a MITM engine, and only while holding
    // `SEAM` (tests are serialized) — no other thread reads the environment concurrently.
    // SSL_CERT_DIR is cleared so it can't take precedence over the file (native-certs
    // loads dirs-then-file when both are present).
    unsafe {
        std::env::set_var("SSL_CERT_FILE", f.path());
        std::env::remove_var("SSL_CERT_DIR");
    }
    f
});

/// Acquire the serialized test seam: lock `SEAM` and force the `SSL_CERT_FILE` trust
/// anchor into place. Held for the whole test.
fn seam() -> MutexGuard<'static, ()> {
    let guard = SEAM.lock().unwrap_or_else(|e| e.into_inner());
    LazyLock::force(&SSL_CERT_FILE);
    guard
}

// ── the in-process TLS upstream (the "real server" the proxy forwards to) ───────

struct Upstream {
    port: u16,
    /// Every complete request's `Authorization` value, in arrival order. EMPTY when the
    /// proxy never forwarded (fail-closed) — the load-bearing negative signal.
    seen_auth: Arc<Mutex<Vec<String>>>,
}

/// Start a TLS server on `127.0.0.1:0` presenting a `localhost` leaf signed by `ca`. It
/// reads one HTTP/1.1 request, records its `Authorization`, and replies 200. Signing with
/// the TRUSTED ca (in `SSL_CERT_FILE`) makes the proxy's upstream verify PASS; signing
/// with a rogue ca makes it FAIL (fail-closed path). Accept threads are detached.
fn tls_upstream(ca: &TestCa) -> Upstream {
    let (chain, key) = ca.leaf_for("localhost");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let cfg = Arc::new(cfg);

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen_auth: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = seen_auth.clone();

    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(tcp) = conn else { continue };
            let cfg = cfg.clone();
            let seen = seen.clone();
            std::thread::spawn(move || {
                let _ = serve_one(tcp, cfg, seen);
            });
        }
    });
    Upstream { port, seen_auth }
}

/// Terminate one client TLS connection, read a single request head, record its
/// `Authorization`, and answer 200. A handshake/read failure (the fail-closed case, where
/// the proxy aborts BEFORE forwarding) returns `Err` and records NOTHING.
fn serve_one(
    mut tcp: TcpStream,
    cfg: Arc<rustls::ServerConfig>,
    seen: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut conn =
        rustls::ServerConnection::new(cfg).map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);

    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]);
            let auth = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Authorization:")
                        .map(|v| v.trim().to_string())
                })
                .unwrap_or_default();
            seen.lock().unwrap().push(auth);
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::other("request head too large"));
        }
        let n = tls.read(&mut tmp)?;
        if n == 0 {
            return Err(std::io::Error::other("client closed before a full head"));
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")?;
    let _ = tls.flush();
    Ok(())
}

// ── the sandboxed child ──────────────────────────────────────────────────────────

/// The node child: assert (into a report file) that its env holds no secret and the CA
/// bundle is a readable temp file, then drive an HTTPS request THROUGH the proxy (manual
/// CONNECT + TLS over the tunnel, core modules only — version-robust, no undici/proxy
/// dependence). Exit 0 only on a verified TLS handshake + a 200.
const CHILD_JS: &str = r#"
const net = require('net'), tls = require('tls'), fs = require('fs');
// `node -e SCRIPT a b c` puts positionals at argv[1..] (no script name under -e).
const [target, reportPath, needle, decoy] = process.argv.slice(1);
const u = new URL(process.env.HTTP_PROXY);
const auth = 'Basic ' + Buffer.from(`${u.username}:`).toString('base64');
// Set once the child completes its TLS handshake WITH THE PROXY (the minted leaf
// verified). Reported always so fail-closed can prove the child got past its own
// handshake — i.e. an empty upstream is the verify gate, not an early child failure.
let secured = false;
function report(extra) {
  const caPath = process.env.NODE_EXTRA_CA_CERTS || '';
  let caReadable = false;
  try { caReadable = fs.readFileSync(caPath, 'utf8').includes('BEGIN CERTIFICATE'); } catch {}
  const secretInEnv = Object.values(process.env).some(v => typeof v === 'string' && v.includes(needle));
  try { fs.writeFileSync(reportPath, JSON.stringify(Object.assign({ caPath, caReadable, secretInEnv, secured }, extra))); } catch {}
}
function fail(code, why) { report({ ok: false, why }); process.exit(code); }
const s = net.connect(parseInt(u.port, 10), '127.0.0.1');
s.on('error', e => fail(3, 'proxy:' + e.message));
let head = '';
function onConnectResp(d) {
  head += d.toString('latin1');
  if (!head.includes('\r\n\r\n')) { s.once('data', onConnectResp); return; }
  if (!head.startsWith('HTTP/1.1 200')) return fail(4, 'connect:' + head.split('\r\n')[0]);
  const t = tls.connect({ socket: s, servername: 'localhost' }, () => {
    t.write('GET / HTTP/1.1\r\nHost: localhost\r\n' +
            'Authorization: ' + decoy + '\r\nConnection: close\r\n\r\n');
    secured = true; // handshake with the proxy done AND the request written
  });
  let resp = '';
  t.on('data', d => resp += d.toString('latin1'));
  t.on('error', e => fail(5, 'tls:' + e.message));
  t.on('end', () => {
    const status = resp.split('\r\n')[0] || '';
    const ok = status.startsWith('HTTP/1.1 200');
    report({ ok, tlsAuthorized: t.authorized, status });
    process.exit(ok ? 0 : 6);
  });
}
s.on('connect', () => s.write(
  `CONNECT ${target} HTTP/1.1\r\nHost: ${target}\r\nProxy-Authorization: ${auth}\r\n\r\n`));
s.once('data', onConnectResp);
setTimeout(() => fail(7, 'timeout'), 12000);
"#;

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

    /// Compile + apply `surface`, then spawn `node -e CHILD_JS` under the sandbox and wait.
    /// Returns the child exit code; the proxy (and its ephemeral CA) is dropped when
    /// `status()` returns, so a post-call `caPath` check proves bundle removal.
    fn run_child(&self, surface: Value, args: &[&str]) -> i32 {
        let policy = compile(&surface, &self.ctx()).expect("compiles");
        let mut spec_args: Vec<&str> = vec!["-e", CHILD_JS];
        spec_args.extend_from_slice(args);
        let spec = CommandSpec::new(NODE).args(spec_args).cwd(&self.proj);
        let prepared = apply(&policy, spec).expect("apply");
        prepared.status().expect("spawn").code().unwrap_or(-1)
    }

    fn report(&self) -> Value {
        let raw = std::fs::read_to_string(self.proj.join("report.json"))
            .expect("child wrote report.json");
        serde_json::from_str(&raw).expect("report is JSON")
    }
}

/// The broker surface: `localhost` injects the real secret. `fs: true` lets node exec +
/// write its report (the CA-bundle read grant is added by the backend); env is left
/// unlisted so it FLOORS to a scrubbed minimum — the child must not carry the secret.
fn broker_surface() -> Value {
    json!({ "fs": true, "net": { "localhost": { "inject": { "Authorization": SECRET } } } })
}

fn preconditions() -> bool {
    if skip_without_landlock(4) {
        return false; // per-host egress + the connect-notify supervisor need ABI v4
    }
    if ptrace_scope() > 1 {
        return false; // the supervisor reads the child's /proc/<pid>/mem
    }
    Path::new(NODE).exists()
}

// ── tests ────────────────────────────────────────────────────────────────────────

#[test]
fn broker_injects_server_side_scrubs_child_and_uses_ephemeral_bundle() {
    let _seam = seam();
    if !preconditions() {
        return;
    }
    let f = fixture();
    let upstream = tls_upstream(&TRUSTED_CA);
    let target = format!("localhost:{}", upstream.port);

    let code = f.run_child(
        broker_surface(),
        &[&target, "report.json", NEEDLE, CHILD_DECOY],
    );
    let report = f.report();

    assert_eq!(
        code, 0,
        "child must complete a verified 200 through the broker: {report}"
    );

    // (2) server-side injection + strip-then-set: the upstream saw the REAL secret and
    // NEVER the child's decoy — proving the child's copy was stripped, the real one set.
    let seen = upstream.seen_auth.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "upstream must receive exactly one request, got {seen:?}"
    );
    assert_eq!(seen[0], SECRET, "upstream must receive the injected secret");
    assert_ne!(
        seen[0], CHILD_DECOY,
        "the child's own Authorization must never be forwarded"
    );

    // (1) the child never held the secret in its (scrubbed) environment.
    assert_eq!(
        report["secretInEnv"],
        json!(false),
        "the secret must not reach the child env"
    );

    // (3a) trust reached the child ONLY via a temp NODE_EXTRA_CA_CERTS bundle it could
    // read, and the child TLS-verified the minted leaf against it (authorized).
    let ca_path = report["caPath"].as_str().unwrap_or_default();
    assert!(
        !ca_path.is_empty(),
        "child must receive a NODE_EXTRA_CA_CERTS bundle"
    );
    assert!(
        !ca_path.starts_with("/etc/") && !ca_path.starts_with("/usr/"),
        "the CA bundle must be a temp file, never the OS trust store: {ca_path}"
    );
    assert_eq!(
        report["caReadable"],
        json!(true),
        "the child must be able to read the bundle"
    );
    assert_eq!(
        report["tlsAuthorized"],
        json!(true),
        "the child must verify the leaf via the bundle"
    );

    // (3b) ephemeral: the bundle is gone once the proxy (dropped in status()) tears down.
    // Bounded poll — a detached tunnel thread may hold the last CA ref briefly past exit.
    let bundle = PathBuf::from(ca_path);
    let deadline = Instant::now() + Duration::from_secs(5);
    while bundle.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !bundle.exists(),
        "the ephemeral CA bundle must not outlive the run: {ca_path}"
    );
}

#[test]
fn broker_fails_closed_on_unverified_upstream_no_leak() {
    let _seam = seam();
    if !preconditions() {
        return;
    }
    let f = fixture();
    // A rogue upstream whose leaf is signed by a CA that is NOT in SSL_CERT_FILE → the
    // proxy's upstream verify FAILS, so it must drop before forwarding (never inject over
    // an unverified channel).
    let rogue_ca = TestCa::generate("rogue upstream CA");
    let upstream = tls_upstream(&rogue_ca);
    let target = format!("localhost:{}", upstream.port);

    let code = f.run_child(
        broker_surface(),
        &[&target, "report.json", NEEDLE, CHILD_DECOY],
    );
    let report = f.report();

    assert_ne!(
        code, 0,
        "child must NOT get a 200 when the upstream is unverified: {report}"
    );
    // The child completed its OWN handshake with the proxy and wrote its request — so the
    // empty upstream below is decisively the proxy's upstream-verify gate blocking the
    // forward, NOT the child failing early (which would make `seen` empty for a benign
    // reason). This makes the leak check self-contained, not cross-test-implied.
    assert_eq!(
        report["secured"],
        json!(true),
        "child must reach the request-send stage so the empty upstream proves the gate: {report}"
    );
    // The decisive leak check: the rogue upstream received NO forwarded request, so the
    // credential (and even the child's decoy) never crossed the unverified channel.
    let seen = upstream.seen_auth.lock().unwrap().clone();
    assert!(
        seen.is_empty(),
        "no request may reach an unverified upstream, saw {seen:?}"
    );
    assert_eq!(
        report["secretInEnv"],
        json!(false),
        "the secret must not reach the child env"
    );
}

#[test]
fn wildcard_broker_is_rejected() {
    let _seam = seam();
    // The credential-laundering guard, at the integration boundary (complements the
    // host-agnostic compiler unit): a wildcard broker would inject the secret to any
    // matching subdomain, so it must not compile into a policy.
    let f = fixture();
    let surface = json!({ "net": { "*.example.com": { "inject": { "Authorization": SECRET } } } });
    assert!(
        compile(&surface, &f.ctx()).is_err(),
        "a wildcard broker must be rejected as a laundering guard"
    );
}
