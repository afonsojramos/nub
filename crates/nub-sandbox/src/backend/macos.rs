//! macOS Seatbelt backend: resolved [`SandboxPolicy`] IR → an SBPL profile,
//! enforced by wrapping the child in `sandbox-exec -p <profile> -- <cmd>`.
//!
//! POSTURE: `(deny default)`. The [`MACOS_SEATBELT_BASE`] block (ported from Codex
//! / Chromium — see the .sbpl header) is the bootstrap that lets an arbitrary
//! binary dyld-load under a deny-default profile; nub then appends the IR-derived
//! read / write / net rules. SBPL is LAST-MATCH-WINS (verified on macOS 26), so a
//! later nub deny overrides an earlier allow — the IR's last-match-wins evaluation
//! order maps onto SBPL emission order 1:1, per axis.
//!
//! Axis mapping:
//!   - reads:  base essential reads always; `default_effect == Allow` adds a
//!     `(allow file-read* (subpath "/"))` generous base; each IR entry emits a
//!     read allow/deny in order. `file-map-executable` shadows every read-allow so
//!     dylibs in an allowed region load.
//!   - writes: deny-default (the base denies all writes); a ReadWrite allow emits
//!     `(allow file-write*)`, a Read allow or a Deny emits `(deny file-write*)` so
//!     a narrower read-only/deny caps a broader earlier write grant.
//!   - net:    not-enforced → `(allow network*)`; enforced WITH a proxy → egress
//!     permitted ONLY to the proxy's loopback port (per-host enforced through it);
//!     enforced WITHOUT a proxy → the base deny stands (coarse deny, loopback closed).
//!   - env:    the child env IS the policy's constructed map (construction, not an
//!     SBPL primitive — a withheld var is simply absent). BUT a scrubbed secret is
//!     only genuinely withheld if the child cannot RECOVER it from a co-resident
//!     same-uid process's environment via `sysctl KERN_PROCARGS2` — so when the
//!     policy withholds a secret we MUST emit an SBPL profile carrying the env-read
//!     closure (below), even if fs/net are relaxed. The closure is the macOS twin of
//!     the Linux `/proc`-close + `ptrace`-deny.
//!
//! CANONICALIZATION: the IR matchers are already firmlink-resolved on their literal
//! prefix by the compiler (`canonicalize_glob_prefix`), and Seatbelt checks the
//! CANONICAL path — so a `/tmp/…` (firmlink) allow that was NOT canonicalized would
//! be inert (silently denied). The confstr scratch dirs this backend adds ARE
//! canonicalized here (incl. not-yet-existing) for the same reason.

use crate::backend::{CommandSpec, Degradation, Prepared};
use crate::matcher::path::canonicalize_including_nonexistent;
use crate::matcher::path::normalize_slashes;
use crate::policy::{Effect, FsAccess, SandboxPolicy};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The bootstrap essential block (`(deny default)` + process/mach/sysctl/iokit +
/// framework map + system read surface). See the .sbpl header for provenance.
const MACOS_SEATBELT_BASE: &str = include_str!("macos_seatbelt_base.sbpl");

/// Mach/socket services real networking needs beyond raw `connect` — DNS resolution
/// (mDNSResponder / SystemConfiguration), TLS trust (trustd / ocspd / SecurityServer),
/// route lookup. Emitted only when net is fully allowed (not-enforced); loopback-only
/// egress needs none of it. Ported from Codex's `seatbelt_network_policy.sbpl`.
const NETWORK_SERVICES: &str = "\
(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))
(allow mach-lookup
  (global-name \"com.apple.bsd.dirhelper\")
  (global-name \"com.apple.system.opendirectoryd.membership\")
  (global-name \"com.apple.SecurityServer\")
  (global-name \"com.apple.networkd\")
  (global-name \"com.apple.ocspd\")
  (global-name \"com.apple.trustd\")
  (global-name \"com.apple.trustd.agent\")
  (global-name \"com.apple.SystemConfiguration.DNSConfiguration\")
  (global-name \"com.apple.SystemConfiguration.configd\")
  (global-name \"com.apple.dnssd.service\")
  (global-name \"com.apple.mDNSResponder.dnsproxy\")
  (global-name \"com.apple.mDNSResponder.uds\"))
(allow sysctl-read (sysctl-name-regex #\"^net.routetable\"))
";

/// Apply a resolved policy to a command on macOS. When the policy confines neither
/// fs nor net, no SBPL wrap is emitted (env-scrub alone is construction, needs no
/// kernel primitive); otherwise the child is re-homed under `sandbox-exec`.
pub fn apply(
    policy: &SandboxPolicy,
    spec: CommandSpec,
    proxy_port: Option<u16>,
) -> Result<Prepared, Degradation> {
    if !needs_wrap(policy) {
        // Nothing to confine and no withheld secret to protect: the env-scrub is pure
        // construction (the child gets exactly the constructed map), so no SBPL profile
        // is needed. env is HONESTLY full here — no secret is being denied the child,
        // hence nothing to recover cross-process. (When a secret IS withheld,
        // `needs_wrap` is true and we fall through to emit the env-read closure below.)
        return Ok(Prepared {
            command: base_command(&spec, policy),
            degradation: Degradation::full(),
            proxy: None,
        });
    }

    let profile = build_profile(policy, &spec, proxy_port);
    let mut wrapped = Command::new("sandbox-exec");
    wrapped.arg("-p").arg(&profile).arg("--");
    wrapped.arg(&spec.program).args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        wrapped.current_dir(cwd);
    }
    // Env-scrub is CONSTRUCTION: the wrapped `sandbox-exec` Command would otherwise
    // inherit this process's full parent env at spawn — re-leaking every secret the
    // scrub removed. Clear it and set exactly the constructed map. (Ported hard-won
    // fix: a fresh Command inherits the parent environ, so env_clear is mandatory.)
    if policy.env.enforce {
        wrapped.env_clear();
        for (k, v) in &policy.env.constructed {
            wrapped.env(k, v);
        }
    }
    // Point the child at the loopback proxy (cooperative hint; the Seatbelt carve is
    // the real boundary). Set AFTER env_clear so it survives an enforced env scrub.
    if let Some(port) = proxy_port {
        super::set_proxy_env(&mut wrapped, port);
    }

    Ok(Prepared {
        command: wrapped,
        degradation: degradation(policy, proxy_port),
        proxy: None,
    })
}

/// The unwrapped command (program + args + cwd + env-scrub) for the no-confinement
/// path. The env axis is enforced by construction here exactly as in the skeleton.
fn base_command(spec: &CommandSpec, policy: &SandboxPolicy) -> Command {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if policy.env.enforce {
        command.env_clear();
        for (k, v) in &policy.env.constructed {
            command.env(k, v);
        }
    }
    command
}

/// Whether an SBPL profile must be emitted. Beyond fs/net confinement, a policy that
/// WITHHOLDS an env secret also requires a profile: the env-read closure that stops
/// the child recovering that secret from a co-resident process's environment lives in
/// the SBPL, so an env-only scrub that hides a secret is not genuinely enforced
/// without a wrap. (Mirrors the Linux backend, where `env.enforce` likewise engages
/// the sandbox.) This is what keeps `is_full()` honest: every path that withholds a
/// secret wraps, so none can report full env enforcement while leaving procargs2 open.
fn needs_wrap(policy: &SandboxPolicy) -> bool {
    needs_sandbox(policy) || env_needs_closure(policy)
}

/// A profile is emitted for an fs or net axis to enforce. A fully relaxed fs +
/// non-enforcing net needs no kernel confinement (on its own).
fn needs_sandbox(policy: &SandboxPolicy) -> bool {
    fs_confines(policy) || policy.net.enforce
}

/// Whether the env axis has a secret to protect cross-process. A passthrough
/// `{env:true}` (enforce set but nothing withheld) denies the child nothing, so there
/// is no secret to recover from a sibling — the env-read closure is unnecessary and we
/// need not wrap for it. Only a scrub that actually WITHHOLDS a var creates the
/// recovery surface the closure shuts.
fn env_needs_closure(policy: &SandboxPolicy) -> bool {
    policy.env.enforce && !policy.env.withheld.is_empty()
}

/// Whether the fs axis confines anything. A relaxed axis is `default_effect ==
/// Allow` with no entries (allow everything); anything else confines.
fn fs_confines(policy: &SandboxPolicy) -> bool {
    policy.fs.rules.default_effect != Effect::Allow || !policy.fs.rules.entries.is_empty()
}

/// Build the full SBPL profile text for `policy`.
fn build_profile(policy: &SandboxPolicy, spec: &CommandSpec, proxy_port: Option<u16>) -> String {
    let mut out = String::with_capacity(MACOS_SEATBELT_BASE.len() + 2048);
    out.push_str(MACOS_SEATBELT_BASE);
    out.push('\n');

    emit_env_read_closure(&mut out);
    emit_net(policy, proxy_port, &mut out);
    emit_fs(policy, spec, &mut out);

    out
}

/// The macOS env-read closure — the load-bearing security default that stops a
/// confined child recovering a scrubbed secret from a co-resident same-uid process's
/// environment. Emitted UNCONDITIONALLY on every wrapped profile, all macOS versions.
///
/// The vector: `sysctl KERN_PROCARGS2` (and its `KERN_PROCARGS` twin) return a target
/// pid's argv+environ. The kernel permits that read iff, for the target, EITHER
/// `sysctl-read` OR `process-info*` is allowed — a DISJUNCTION, so BOTH arms must be
/// denied. Under this backend's `(deny default)` only the process-info arm is open:
///
/// - sysctl arm: already shut — procargs2's (pid-parameterized, unnameable) sysctl is
///   not in the base allowlist, and the base allows kern.* only by SPECIFIC NAME
///   (never a `(sysctl-name-prefix "kern.")`, which WOULD re-admit it). No sysctl rule
///   is needed here.
/// - process-info arm: OPEN — `process-info*` is allowed-by-default even under
///   `(deny default)`, so it must be denied EXPLICITLY. This is that denial.
///
/// The self-restore is `(target self)` and nothing wider: `(target others)` leaks a
/// sibling's env, and `(target same-sandbox)` re-opens the hole (a confined child's
/// own siblings/children ARE same-sandbox); node needs only self-introspection.
/// Empirically verified 20/20 with negative controls on macOS 26 (xnu-12377).
fn emit_env_read_closure(out: &mut String) {
    out.push_str("(deny process-info*)\n");
    out.push_str("(allow process-info* (target self))\n");
}

/// Net axis. Three cases:
///   - not enforced → allow all egress + the DNS/TLS service block.
///   - enforced WITH a proxy → permit egress ONLY to the proxy's loopback port, so
///     the child must route per-host through it. This deliberately does NOT carve all
///     of loopback: arbitrary local services (a sibling listener, a docker daemon on
///     127.0.0.1) and AF_UNIX sockets (`docker.sock`) stay DENIED by the base — the
///     local-exfil holes the old `localhost:*` carve left open are closed here.
///   - enforced WITHOUT a proxy (coarse deny-all) → NO carve at all; the base
///     `(deny default)` denies every egress including loopback (nothing reachable).
///
/// Seatbelt requires `localhost`/`*` as the host in a `remote ip` literal (a numeric
/// `127.0.0.1` literal is a PARSE ERROR that fails the whole profile load); `localhost`
/// covers loopback on both 127.0.0.1 and ::1, and the explicit `:<port>` pins the one
/// proxy port.
fn emit_net(policy: &SandboxPolicy, proxy_port: Option<u16>, out: &mut String) {
    if !policy.net.enforce {
        out.push_str("(allow network*)\n");
        out.push_str(NETWORK_SERVICES);
        return;
    }
    if let Some(port) = proxy_port {
        out.push_str(&format!(
            "(allow network* (remote ip \"localhost:{port}\"))\n"
        ));
    }
    // else: coarse deny-all — emit nothing (the base (deny default) closes all egress,
    // loopback and AF_UNIX included).
}

/// Filesystem axis: reads then writes, each reproducing the IR's last-match-wins
/// over the same ordered entry list.
fn emit_fs(policy: &SandboxPolicy, spec: &CommandSpec, out: &mut String) {
    if !fs_confines(policy) {
        // Fully relaxed fs — grant every file op (we wrapped only to enforce net).
        out.push_str("(allow file*)\n");
        return;
    }

    // ── reads ────────────────────────────────────────────────────────────────
    if policy.fs.rules.default_effect == Effect::Allow {
        // Unmatched reads allowed (generous base); entries below tighten it.
        out.push_str("(allow file-read* (subpath \"/\"))\n");
        out.push_str("(allow file-map-executable (subpath \"/\"))\n");
    }
    // Auto-grant read/map of the target binary FILE so read-confine can exec it
    // (system tools are already covered by the essential base). Only the file — NOT
    // its parent dir: a directory grant would expose the program's SIBLINGS (e.g. a
    // `.env`/key next to a project-local tool), defeating a tight read allowlist. A
    // non-system toolchain's out-of-dir libs need an explicit toolchain allow.
    if let Some(term) = program_read_term(spec) {
        out.push_str(&format!("(allow file-read* {term})\n"));
        out.push_str(&format!("(allow file-map-executable {term})\n"));
    }
    for rule in &policy.fs.rules.entries {
        let term = emit_term(&to_match_term(rule.matcher.as_str()));
        match rule.effect {
            Effect::Allow => {
                out.push_str(&format!("(allow file-read* {term})\n"));
                out.push_str(&format!("(allow file-map-executable {term})\n"));
            }
            Effect::Deny => out.push_str(&format!("(deny file-read* {term})\n")),
        }
    }

    // ── writes (base denies all writes) ───────────────────────────────────────
    for rule in &policy.fs.rules.entries {
        let m = to_match_term(rule.matcher.as_str());
        let term = emit_term(&m);
        match (rule.effect, rule.access) {
            (Effect::Allow, FsAccess::ReadWrite) => {
                // Refuse a write grant that resolves to a dangerous top-level root
                // (a `..` in a surface path can collapse a grant up to `/private`
                // etc. — an accidental filesystem-wide write hole). Fail-safe: drop
                // the over-broad grant rather than emit it.
                if is_dangerous_write_root(&m) {
                    continue;
                }
                out.push_str(&format!("(allow file-write* {term})\n"));
            }
            // A read-only allow or a deny caps write: revoke any write a broader
            // earlier rw-allow granted at this path (last-match-wins).
            (Effect::Allow, FsAccess::Read) | (Effect::Deny, _) => {
                out.push_str(&format!("(deny file-write* {term})\n"))
            }
        }
    }
    // The Apple toolchain (xcrun/cc/libtool) writes its `xcrun_db` scratch to the
    // per-user DARWIN confstr TEMP dir — NOT redirectable via TMPDIR — so a
    // from-source compile fails without this grant. Emitted LAST so it survives a
    // generous-read policy's `(deny file-write* /)` cap (last-match-wins); the
    // only thing it can override is a user write-deny targeting the OS temp, which
    // is rare and acceptable. The persistent DARWIN CACHE dir is deliberately NOT
    // granted — it is a cross-build poisoning surface a later unsandboxed tool
    // consumes, and `cc`/`xcrun` need only the temp scratch.
    for dir in confstr_scratch_dirs() {
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(&dir)
        ));
    }

    emit_move_block(policy, out);
}

/// Close the move/rename secret-relocation bypass (SRT's `generateMoveBlockingRules`).
/// A secret is protected by a write-DENY on its path, but two macOS holes let a child
/// relocate the bytes past that path-keyed deny: (1) the trailing confstr
/// `(allow file-write* <temp>)` grant above is last-match-wins, so it re-opens
/// unlink/rename on any denied path living under `$TMPDIR`; (2) an anchored deny
/// (`/proj/.env`) blocks the file `mv` but not `mv proj proj2`, which relocates the whole
/// containing dir out from under the anchored deny.
///
/// INVARIANT (load-bearing): these denies MUST be emitted AFTER the confstr grant so they
/// win the last-match-wins race, and ONLY the Deny arm + the ancestor-dir chain are
/// re-denied — NEVER the generous `/` read-cap or the confstr grant itself, either of which
/// would re-deny the legit `xcrun_db` / `$TMPDIR` scratch write.
fn emit_move_block(policy: &SandboxPolicy, out: &mut String) {
    // Fix 1 — re-assert each Deny's unlink/create block. A `(subpath)` deny covers the
    // denied file/subtree; re-emitting the unlink/create primitives here restores the deny
    // that the trailing confstr write grant would otherwise override for a `$TMPDIR` secret.
    for rule in &policy.fs.rules.entries {
        if rule.effect == Effect::Deny {
            let term = emit_term(&to_match_term(rule.matcher.as_str()));
            out.push_str(&format!("(deny file-write-unlink {term})\n"));
            out.push_str(&format!("(deny file-write-create {term})\n"));
        }
    }

    // Fix 2 — ancestor move-block for DIRECTORY-PINNING denies. For each deny, pin
    // unlink/create on the directory chain from the secret's innermost writable container
    // up to (and including) the enclosing write-grant root, so renaming a container can't
    // relocate the secret past its path-keyed deny. The chain start differs by deny shape,
    // because Fix 1's re-asserted deny covers a different innermost path in each:
    //   • LITERAL `(subpath)` deny (`/proj/.env`, `/proj/secrets` subtree) — Fix 1's subpath
    //     deny already matches its own root path, so renaming the secret / subtree-root
    //     itself is blocked; only the ANCESTORS need pinning. Probe = the secret path; the
    //     walk pins parent(secret) upward.
    //   • REGEX directory-pinning deny (`!secrets/*.key` → `/proj/secrets/*.key`) — Fix 1's
    //     regex deny matches only the glob LEAF files, NOT their literal container dir
    //     `/proj/secrets`, so `mv secrets secretz` relocates the leaves past the deny. Pin
    //     the deny's literal directory PREFIX itself and up. Probe = `<prefix>/*`, so the
    //     walk pins `<prefix>` (not just its parent) upward.
    // A deny with no absolute literal directory prefix under a grant root (`**/secrets/**` —
    // the matched dir name floats, no fixed anchor) yields nothing to pin; documented as a
    // residual in LIMITATIONS.md. The `(literal P)` denies are EXACT-path — they block
    // renaming dir `P` itself, never a create/write INSIDE it, so `echo > proj/other.txt`
    // and writes under `/proj/secrets/` still work.
    let grant_roots = write_grant_roots(policy);
    for rule in &policy.fs.rules.entries {
        if rule.effect != Effect::Deny {
            continue;
        }
        let probe = match to_match_term(rule.matcher.as_str()) {
            MatchTerm::Subpath(denied) => denied,
            MatchTerm::Regex(_) => {
                let Some(prefix) = regex_literal_dir_prefix(rule.matcher.as_str()) else {
                    continue;
                };
                format!("{prefix}/*")
            }
        };
        for anc in move_block_ancestors(&probe, &grant_roots) {
            let lit = format!("(literal \"{}\")", sbpl_escape(&anc));
            out.push_str(&format!("(deny file-write-unlink {lit})\n"));
            out.push_str(&format!("(deny file-write-create {lit})\n"));
        }
    }
}

/// The literal directory PREFIX of a glob deny — the leading run of glob-free path
/// components (`/proj/secrets/*.key` → `/proj/secrets`; `/proj/packages/*/.env` →
/// `/proj/packages`). This is the deepest directory whose rename relocates a matched
/// secret OUT of the deny: the glob leaf below it stays matched under an in-place rename
/// (`*` re-matches the new name), but renaming the literal prefix escapes the pattern.
/// `None` when there is no absolute multi-component prefix to anchor (a first-segment or
/// leading-`**` glob) — matching the meta set `to_match_term` uses to pick Regex.
fn regex_literal_dir_prefix(glob: &str) -> Option<String> {
    let meta = glob.find(['*', '?', '[', ']', '{', '}'])?;
    let slash = glob[..meta].rfind('/')?;
    let prefix = &glob[..slash];
    (prefix.len() > 1 && prefix.starts_with('/')).then(|| prefix.to_string())
}

/// The write-granted subpath roots: every rw Allow that survives the dangerous-root
/// guard, plus the confstr scratch dirs (also `(allow file-write* (subpath …))` grants).
/// A directory rename can only relocate a secret when the container is writable, so these
/// roots bound how far up the ancestor move-block must reach.
fn write_grant_roots(policy: &SandboxPolicy) -> Vec<String> {
    let mut roots = Vec::new();
    for rule in &policy.fs.rules.entries {
        if let (Effect::Allow, FsAccess::ReadWrite) = (rule.effect, rule.access) {
            let m = to_match_term(rule.matcher.as_str());
            if is_dangerous_write_root(&m) {
                continue;
            }
            if let MatchTerm::Subpath(p) = m {
                roots.push(p);
            }
        }
    }
    roots.extend(confstr_scratch_dirs());
    roots
}

/// Ancestor directories to move-block for an anchored deny at `denied`: from the secret's
/// PARENT up to and including the outermost (shortest) write-grant root that STRICTLY
/// contains it. Empty when no write grant encloses the deny — no writable container to
/// rename, so nothing to block (the base denies write on every ancestor).
fn move_block_ancestors(denied: &str, grant_roots: &[String]) -> Vec<String> {
    let Some(root) = grant_roots
        .iter()
        .filter(|g| path_strictly_contains(g, denied))
        .min_by_key(|g| g.len())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur = parent_dir(denied);
    while let Some(dir) = cur {
        out.push(dir.to_string());
        if dir == root.as_str() {
            break;
        }
        cur = parent_dir(dir);
    }
    out
}

/// Whether `root` is a strict ancestor directory of `child` (`child` == `root` + `/…`).
/// Strict (not equal) so a deny whose path equals a grant root is left to the file-level
/// deny, never walked as its own ancestor.
fn path_strictly_contains(root: &str, child: &str) -> bool {
    child
        .strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The parent directory of a path as a `&str`, or `None` at the filesystem root. Filters
/// the empty parent so a top-level entry doesn't yield `""`.
fn parent_dir(p: &str) -> Option<&str> {
    Path::new(p)
        .parent()
        .and_then(Path::to_str)
        .filter(|s| !s.is_empty())
}

/// Top-level roots a write grant must never cover — a `..`-collapsed surface path
/// (`/tmp/..` → `/private`) would otherwise open filesystem-wide write. Reads are
/// exempt (a generous `(subpath "/")` read is the legitimate default posture).
///
/// The matcher reaching here is already firmlink-CANONICALIZED, so the entries must
/// be the canonical forms the guard actually sees: `/var`/`/etc`/`/tmp` resolve to
/// `/private/var`/`/private/etc`/`/private/tmp`. The firmlink spellings are kept
/// too (harmless, self-documenting); `/private/tmp` is deliberately absent — it is
/// the legitimate temp firmlink target, not a broad system root.
fn is_dangerous_write_root(term: &MatchTerm) -> bool {
    let MatchTerm::Subpath(p) = term else {
        return false;
    };
    matches!(
        p.as_str(),
        "/" | "/private"
            | "/private/var"
            | "/private/etc"
            | "/System"
            | "/Users"
            | "/usr"
            | "/bin"
            | "/sbin"
            | "/etc"
            | "/var"
            | "/opt"
            | "/Library"
            | "/Applications"
            | "/Volumes"
            | "/Network"
            | "/cores"
    )
}

/// Best-effort read/map grant for the target program FILE so read-confine can exec
/// it. `None` when the program can't be resolved (a bare name with no PATH hit) —
/// the essential base still covers system tools.
fn program_read_term(spec: &CommandSpec) -> Option<String> {
    let resolved = resolve_program(&spec.program, spec.cwd.as_deref())?;
    let file = normalize_slashes(&resolved.to_string_lossy());
    Some(format!("(subpath \"{}\")", sbpl_escape(&file)))
}

/// Resolve a program to an absolute, canonical path. A cwd-relative program is
/// resolved against the CHILD's cwd (`spec.cwd`, where the kernel will resolve it),
/// falling back to the process cwd; a bare name is searched on `PATH`.
fn resolve_program(program: &std::ffi::OsStr, child_cwd: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return Some(canonicalize_including_nonexistent(p));
    }
    if p.components().count() > 1 {
        // cwd-relative (`./x`, `dir/x`) — anchor at the child's cwd, not ours.
        let base = match child_cwd {
            Some(c) => c.to_path_buf(),
            None => std::env::current_dir().ok()?,
        };
        return Some(canonicalize_including_nonexistent(&base.join(p)));
    }
    // bare name → PATH search
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(p);
        if cand.is_file() {
            return Some(canonicalize_including_nonexistent(&cand));
        }
    }
    None
}

/// The per-user DARWIN confstr TEMP scratch dir (`/private/var/folders/<uid>/T`),
/// canonicalized (a `/var/folders/…` firmlink resolving under `/private`). Only the
/// TEMP dir — NOT the persistent CACHE dir (`…/C`), which is a cross-build poisoning
/// surface. Empty off macOS or when confstr yields nothing.
fn confstr_scratch_dirs() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dir) = confstr_dir(libc::_CS_DARWIN_USER_TEMP_DIR) {
        let canon = canonicalize_including_nonexistent(&dir);
        let s = normalize_slashes(&canon.to_string_lossy());
        // Refuse a root/empty grant (would be a filesystem-wide write hole).
        if !s.is_empty() && s != "/" {
            out.push(s);
        }
    }
    out
}

/// Query one `confstr(3)` path. Two-call idiom: size probe, then fill.
fn confstr_dir(name: libc::c_int) -> Option<PathBuf> {
    // SAFETY: standard confstr two-call sequence — first a NULL/0 size probe, then
    // a fill into an exactly-sized buffer; the returned string is NUL-terminated.
    unsafe {
        let len = libc::confstr(name, std::ptr::null_mut(), 0);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        let got = libc::confstr(name, buf.as_mut_ptr() as *mut libc::c_char, len);
        if got == 0 || got > len {
            return None;
        }
        // Trim at the NUL and any trailing slash the OS appends.
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).into_owned();
        let s = s.trim_end_matches('/');
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

/// A translated SBPL match term: an absolute-literal subtree, or a glob rendered as
/// an anchored Seatbelt regex.
enum MatchTerm {
    Subpath(String),
    Regex(String),
}

/// Translate one canonical IR glob into an SBPL match term. An absolute literal
/// (or a literal + trailing `/**`) becomes `(subpath …)` — exact and cheap; a
/// whole-fs `**` becomes `(subpath "/")`; anything with embedded globs becomes an
/// anchored regex (Seatbelt has no glob syntax).
fn to_match_term(glob: &str) -> MatchTerm {
    if glob == "**" || glob == "/**" || glob == "/" {
        return MatchTerm::Subpath("/".to_string());
    }
    let has_meta = glob.contains(['*', '?', '[', ']', '{', '}']);
    if !has_meta && glob.starts_with('/') {
        return MatchTerm::Subpath(glob.to_string());
    }
    // Literal prefix + trailing `/**` (the common subtree twin) → subpath of prefix.
    if let Some(prefix) = glob.strip_suffix("/**")
        && !prefix.contains(['*', '?', '[', ']', '{', '}'])
        && prefix.starts_with('/')
    {
        return MatchTerm::Subpath(prefix.to_string());
    }
    MatchTerm::Regex(glob_to_seatbelt_regex(glob))
}

/// Render a [`MatchTerm`] as its SBPL fragment.
fn emit_term(term: &MatchTerm) -> String {
    match term {
        MatchTerm::Subpath(p) => format!("(subpath \"{}\")", sbpl_escape(p)),
        MatchTerm::Regex(r) => format!("(regex #\"{}\")", r.replace('"', "\\\"")),
    }
}

/// Translate a git-style glob into an anchored Seatbelt regex. `**/` spans zero or
/// more components, `**` spans anything, `*`/`?` stay within one component, `[…]`
/// stays a character class. A metachar-free pattern gets a subtree `(/.*)?` suffix.
/// Ported from Codex's `seatbelt_regex_for_unreadable_glob` (Apache-2.0).
fn glob_to_seatbelt_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut regex = String::from("^");
    let mut i = 0;
    let mut saw_glob = false;
    while i < chars.len() {
        let ch = chars[i];
        i += 1;
        match ch {
            '*' => {
                saw_glob = true;
                if chars.get(i) == Some(&'*') {
                    i += 1;
                    if chars.get(i) == Some(&'/') {
                        i += 1;
                        regex.push_str("(.*/)?");
                    } else {
                        regex.push_str(".*");
                    }
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => {
                saw_glob = true;
                regex.push_str("[^/]");
            }
            '[' => {
                saw_glob = true;
                let class_start = i;
                let mut class = String::new();
                let mut closed = false;
                while i < chars.len() {
                    let c = chars[i];
                    i += 1;
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    class.push(c);
                }
                if !closed {
                    // Unterminated `[` → literal, reprocess the rest normally.
                    regex.push_str("\\[");
                    i = class_start;
                    continue;
                }
                regex.push('[');
                let mut it = class.chars();
                if let Some(first) = it.next() {
                    match first {
                        '!' => regex.push('^'),
                        '^' => regex.push_str("\\^"),
                        _ => regex.push(first),
                    }
                }
                for c in it {
                    if c == '\\' {
                        regex.push_str("\\\\");
                    } else {
                        regex.push(c);
                    }
                }
                regex.push(']');
            }
            ']' => {
                saw_glob = true;
                regex.push_str("\\]");
            }
            _ => regex.push_str(&regex_escape_char(ch)),
        }
    }
    if !saw_glob {
        regex.push_str("(/.*)?");
    }
    regex.push('$');
    regex
}

/// Escape a literal char for embedding in a regex. `/` and ordinary chars pass
/// through; the glob metachars (`*?[]`) never reach here as literals.
fn regex_escape_char(c: char) -> String {
    match c {
        '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => format!("\\{c}"),
        _ => c.to_string(),
    }
}

/// Escape a path for an SBPL double-quoted string literal.
fn sbpl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The degradation for a WRAPPED profile. env is genuinely enforced on this path (the
/// profile carries the unconditional env-read closure), so it is never reported lost —
/// and the `!needs_wrap` early-return only fires when no secret is withheld, so no path
/// reports full env enforcement while leaving procargs2 open. The one degradable axis
/// is net-per-host: if net enforces per-host allows but the proxy could NOT be started
/// (`proxy_port == None`) the profile coarse-denies and we report `net-per-host`
/// degraded (fail-safe, not silent). With a proxy the per-host allows ARE enforced (via
/// SNI/target gating), so enforcement is full.
fn degradation(policy: &SandboxPolicy, proxy_port: Option<u16>) -> Degradation {
    let mut deg = Degradation::full();
    if policy.net.enforce
        && proxy_port.is_none()
        && policy.net.rules.iter().any(|r| r.effect == Effect::Allow)
    {
        deg.lost.push("net-per-host".to_string());
        deg.reason = Some(
            "egress proxy unavailable — per-host allows denied (coarse network deny)".to_string(),
        );
    }
    deg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{
        CanonGlob, FsPolicy, FsRule, FsRuleSet, NetPolicy, NetRule, NetTarget, TmpMode,
    };

    fn spec() -> CommandSpec {
        CommandSpec::new("/bin/cat")
    }

    fn fs_policy(default_effect: Effect, entries: Vec<FsRule>) -> SandboxPolicy {
        SandboxPolicy {
            fs: FsPolicy {
                rules: FsRuleSet {
                    entries,
                    default_effect,
                },
                tmp: TmpMode::Shared,
            },
            ..Default::default()
        }
    }

    fn rule(m: &str, effect: Effect, access: FsAccess) -> FsRule {
        FsRule {
            matcher: CanonGlob(m.to_string()),
            effect,
            access,
        }
    }

    fn term_str(glob: &str) -> String {
        emit_term(&to_match_term(glob))
    }

    // ── matcher translation ──────────────────────────────────────────────────

    #[test]
    fn whole_fs_globs_become_root_subpath() {
        // The generous-read `**` entry and its `/**`/`/` spellings all mean "all".
        assert_eq!(term_str("**"), "(subpath \"/\")");
        assert_eq!(term_str("/**"), "(subpath \"/\")");
        assert_eq!(term_str("/"), "(subpath \"/\")");
    }

    #[test]
    fn absolute_literal_and_subtree_twin_become_subpath() {
        assert_eq!(term_str("/proj/data"), "(subpath \"/proj/data\")");
        // The `/**` subtree twin collapses to the same subpath (subpath already
        // covers descendants) — the two IR rows map to one grant.
        assert_eq!(term_str("/proj/data/**"), "(subpath \"/proj/data\")");
    }

    #[test]
    fn embedded_globs_become_anchored_regex() {
        // The depth-independent `.env` denies (the security-critical case).
        assert_eq!(term_str("**/.env"), "(regex #\"^(.*/)?\\.env$\")");
        assert_eq!(term_str("**/.env.*"), "(regex #\"^(.*/)?\\.env\\.[^/]*$\")");
        // A single-component glob stays within one path segment.
        assert_eq!(term_str("/proj/*.pem"), "(regex #\"^/proj/[^/]*\\.pem$\")");
        // A mid-path single `*` does not cross a separator.
        assert_eq!(
            term_str("/proj/packages/*/.env"),
            "(regex #\"^/proj/packages/[^/]*/\\.env$\")"
        );
    }

    // ── profile shape ────────────────────────────────────────────────────────

    #[test]
    fn read_generous_emits_root_allow_then_secret_deny() {
        // `sandbox: true`-shaped: a `**` allow (generous) then a `.env` deny.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("**/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(allow file-read* (subpath \"/\"))"));
        // The `.env` deny is emitted AFTER the generous allow (last-match-wins).
        let allow_at = prof.find("(allow file-read* (subpath \"/\"))").unwrap();
        let deny_at = prof
            .find("(deny file-read* (regex #\"^(.*/)?\\.env$\"))")
            .unwrap();
        assert!(
            deny_at > allow_at,
            "the .env deny must follow the generous allow"
        );
    }

    #[test]
    fn read_confine_has_no_global_read_allow() {
        // default_effect Deny + explicit project allow = read-confine; unmatched
        // paths fall through to the base `(deny default)`, not a global read allow.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(allow file-read* (subpath \"/\"))\n"));
        assert!(prof.contains("(allow file-read* (subpath \"/proj\"))"));
    }

    #[test]
    fn write_axis_maps_access_to_allow_or_capping_deny() {
        // rw → write allow; read-only allow → write deny (caps a broader grant);
        // deny → write deny. Base denies writes, so only rw opens one.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("/proj", Effect::Allow, FsAccess::ReadWrite),
                rule("/proj/ro", Effect::Allow, FsAccess::Read),
                rule("/proj/secret", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(allow file-write* (subpath \"/proj\"))"));
        assert!(prof.contains("(deny file-write* (subpath \"/proj/ro\"))"));
        assert!(prof.contains("(deny file-write* (subpath \"/proj/secret\"))"));
    }

    #[test]
    fn confstr_scratch_write_wins_over_generous_write_cap() {
        // A generous-read policy caps writes with `(deny file-write* (subpath "/"))`
        // (from the `**` read-only allow). The confstr temp grant MUST be emitted
        // after it so it survives last-match-wins — otherwise the Apple toolchain's
        // xcrun_db write is silently denied (the C1 regression).
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/proj", Effect::Allow, FsAccess::ReadWrite),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        let cap = prof.find("(deny file-write* (subpath \"/\"))").unwrap();
        let confstr = prof
            .find("(allow file-write* (subpath \"/private/var/folders/")
            .unwrap();
        assert!(
            confstr > cap,
            "confstr grant must follow the write cap-deny"
        );
    }

    #[test]
    fn move_block_reasserts_deny_after_confstr_grant() {
        // Hole #1: a `.env` deny under a generous-read policy is capped by
        // `(deny file-write* <.env>)`, but the trailing confstr grant re-opens write for a
        // `$TMPDIR`-resident secret (last-match-wins). The move-block re-emits the
        // unlink/create denies AFTER the confstr grant so the deny wins the race.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("**/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        let confstr = prof
            .find("(allow file-write* (subpath \"/private/var/folders/")
            .expect("confstr temp grant present");
        let unlink = prof
            .find("(deny file-write-unlink (regex #\"^(.*/)?\\.env$\"))")
            .expect("re-asserted unlink deny present");
        let create = prof
            .find("(deny file-write-create (regex #\"^(.*/)?\\.env$\"))")
            .expect("re-asserted create deny present");
        assert!(
            unlink > confstr && create > confstr,
            "move-block denies must follow the confstr grant to win last-match-wins"
        );
    }

    #[test]
    fn move_block_does_not_reassert_generous_write_cap() {
        // The `**` read-only allow emits `(deny file-write* (subpath "/"))`; re-asserting
        // THAT after the confstr grant would re-deny the whole temp dir and break the
        // xcrun_db write. Only the Deny arm is re-emitted — no root-subpath unlink/create
        // deny may appear (which would blanket-block the confstr scratch write).
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/proj", Effect::Allow, FsAccess::ReadWrite),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(deny file-write-unlink (subpath \"/\"))"));
        assert!(!prof.contains("(deny file-write-create (subpath \"/\"))"));
        // And the confstr grant is still the last word on the temp dir.
        assert!(prof.contains("(allow file-write* (subpath \"/private/var/folders/"));
    }

    #[test]
    fn move_block_denies_ancestor_dirs_for_anchored_deny() {
        // Hole #2: a literal deny `/root/proj/.env` blocks the file mv but not
        // `mv proj proj2`. The ancestor move-block denies unlink/create on `/root/proj`
        // and `/root` (up to the rw-grant root), so no container rename relocates it.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/root", Effect::Allow, FsAccess::ReadWrite),
                rule("/root/proj/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(deny file-write-unlink (literal \"/root/proj\"))"));
        assert!(prof.contains("(deny file-write-create (literal \"/root/proj\"))"));
        assert!(prof.contains("(deny file-write-unlink (literal \"/root\"))"));
        // The grant root is the stopping point — nothing above it (writable region ends).
        assert!(!prof.contains("(deny file-write-unlink (literal \"/\"))"));
    }

    #[test]
    fn move_block_skips_basename_glob_deny_ancestors() {
        // A basename-glob deny (`**/.env`) has no literal ancestor and is already immune to
        // ancestor rename (the basename survives), so Fix 2 emits no `(literal …)` ancestor
        // denies for it — only the Fix 1 regex re-assertion.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/root", Effect::Allow, FsAccess::ReadWrite),
                rule("**/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(deny file-write-unlink (literal \"/root\"))"));
    }

    #[test]
    fn move_block_pins_regex_dir_prefix_ancestors() {
        // A user directory-pinning glob deny (`!secrets/*.key` → `/root/secrets/*.key`) is a
        // regex, so Fix 1 blocks the leaf `*.key` files but NOT their container `/root/secrets`
        // — `mv secrets secretz` would relocate them past the deny. Fix 2 pins the literal
        // prefix dir `/root/secrets` AND its ancestors up to the rw-grant root.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/root", Effect::Allow, FsAccess::ReadWrite),
                rule("/root/secrets/*.key", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(deny file-write-unlink (literal \"/root/secrets\"))"));
        assert!(prof.contains("(deny file-write-create (literal \"/root/secrets\"))"));
        assert!(prof.contains("(deny file-write-unlink (literal \"/root\"))"));
        // EXACT-path, never a subpath — a legit write UNDER secrets/ stays permitted.
        assert!(!prof.contains("(literal \"/root/secrets/"));
        // The grant root is the stopping point — nothing above it.
        assert!(!prof.contains("(deny file-write-unlink (literal \"/\"))"));
    }

    #[test]
    fn regex_literal_dir_prefix_extracts_leading_literal_run() {
        // The leading glob-free component run, dropping the glob leaf/segment.
        assert_eq!(
            regex_literal_dir_prefix("/root/secrets/*.key").as_deref(),
            Some("/root/secrets")
        );
        assert_eq!(
            regex_literal_dir_prefix("/root/packages/*/.env").as_deref(),
            Some("/root/packages")
        );
        // No fixed anchor: a leading `**` (basename/floating glob) or a first-segment glob.
        assert_eq!(regex_literal_dir_prefix("**/.env"), None);
        assert_eq!(regex_literal_dir_prefix("/*.key"), None);
    }

    #[test]
    fn move_block_no_regex_pin_without_literal_prefix() {
        // A floating-name deny (`**/secrets/**`) has no absolute literal prefix to anchor, so
        // Fix 2 emits no `(literal …)` ancestor denies for it — the documented residual.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/root", Effect::Allow, FsAccess::ReadWrite),
                rule("**/secrets/**", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(deny file-write-unlink (literal \"/root\"))"));
    }

    #[test]
    fn move_block_no_ancestors_without_enclosing_write_grant() {
        // An anchored deny with NO write grant enclosing it (read-only project) has no
        // writable container to rename — emit no ancestor denies.
        let p = fs_policy(
            Effect::Deny,
            vec![
                rule("**", Effect::Allow, FsAccess::Read),
                rule("/root/proj/.env", Effect::Deny, FsAccess::Read),
            ],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(deny file-write-unlink (literal \"/root/proj\"))"));
        assert!(!prof.contains("(deny file-write-unlink (literal \"/root\"))"));
    }

    #[test]
    fn confstr_grants_temp_not_cache() {
        // Only the DARWIN TEMP dir is granted; the persistent CACHE dir (…/C) is a
        // cross-build poisoning surface and must NOT be write-granted.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec(), None);
        if let Some(cache) = confstr_dir(libc::_CS_DARWIN_USER_CACHE_DIR) {
            let cache =
                normalize_slashes(&canonicalize_including_nonexistent(&cache).to_string_lossy());
            assert!(
                !prof.contains(&format!("(allow file-write* (subpath \"{cache}\"))")),
                "the DARWIN cache dir must not be write-granted"
            );
        }
    }

    #[test]
    fn dangerous_write_roots_are_dropped() {
        // A `..`-collapsed grant that resolves to a top-level root must not emit a
        // write allow (filesystem-wide write hole). Read of `/` stays legal.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/private", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(!prof.contains("(allow file-write* (subpath \"/private\"))"));
        assert!(is_dangerous_write_root(&MatchTerm::Subpath(
            "/private".to_string()
        )));
        // The canonical forms of firmlink roots (`/var`→`/private/var`) — what the
        // guard actually sees after the matcher's canonicalization — must be caught.
        assert!(is_dangerous_write_root(&MatchTerm::Subpath(
            "/private/var".to_string()
        )));
        assert!(is_dangerous_write_root(&MatchTerm::Subpath(
            "/private/etc".to_string()
        )));
        assert!(is_dangerous_write_root(&MatchTerm::Subpath(
            "/Volumes".to_string()
        )));
        // A real project dir under a guarded root is NOT over-blocked (exact match).
        assert!(!is_dangerous_write_root(&MatchTerm::Subpath(
            "/proj".to_string()
        )));
        assert!(!is_dangerous_write_root(&MatchTerm::Subpath(
            "/Users/me/proj".to_string()
        )));
        assert!(!is_dangerous_write_root(&MatchTerm::Subpath(
            "/private/tmp/scratch".to_string()
        )));
    }

    #[test]
    fn relaxed_fs_grants_all_file_ops() {
        // default Allow + no entries = relaxed; wrapped only because net enforces.
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![],
            default_effect: Effect::Deny,
        };
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(allow file*)"));
    }

    #[test]
    fn net_enforced_with_proxy_carves_only_the_proxy_port() {
        // A proxy on port 54321: egress permitted to EXACTLY localhost:54321, nothing
        // else — no blanket allow, and critically NOT all-loopback (`localhost:*`), so
        // a sibling listener / docker-on-loopback stays denied (local-exfil closed).
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![],
            default_effect: Effect::Deny,
        };
        let prof = build_profile(&p, &spec(), Some(54321));
        assert!(prof.contains("(allow network* (remote ip \"localhost:54321\"))"));
        assert!(
            !prof.contains("localhost:*"),
            "must not carve all of loopback"
        );
        assert!(!prof.contains("(allow network*)\n"), "no blanket egress");
    }

    #[test]
    fn net_enforced_coarse_deny_carves_nothing() {
        // Coarse deny-all (net enforce, no proxy): NO network allow at all — the base
        // (deny default) closes every egress incl. loopback + AF_UNIX.
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![],
            default_effect: Effect::Deny,
        };
        let prof = build_profile(&p, &spec(), None);
        assert!(
            !prof.contains("(allow network*"),
            "coarse deny emits no egress carve"
        );
    }

    #[test]
    fn net_not_enforced_allows_all_plus_services() {
        // fs confines (so we wrap) but net is relaxed → full egress + DNS/TLS block.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(allow network*)\n"));
        assert!(prof.contains("com.apple.trustd"));
    }

    #[test]
    fn degradation_reports_lost_per_host_only_without_proxy() {
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![NetRule {
                target: NetTarget::Host("example.com".to_string()),
                effect: Effect::Allow,
            }],
            default_effect: Effect::Deny,
        };
        // No proxy available → per-host can't be enforced → degraded.
        let deg = degradation(&p, None);
        assert_eq!(deg.lost, vec!["net-per-host".to_string()]);
        // WITH a proxy the per-host allows ARE enforced (SNI/target gating) → full.
        assert!(degradation(&p, Some(9999)).is_full());
        // A pure deny-all net (no allow rules) is fully enforced, not degraded.
        p.net.rules.clear();
        assert!(degradation(&p, None).is_full());
    }

    #[test]
    fn no_sandbox_wrap_when_nothing_confines() {
        // Relaxed fs + non-enforcing net + no env secret = env-scrub only, no SBPL.
        let p = fs_policy(Effect::Allow, vec![]);
        assert!(!needs_sandbox(&p));
        assert!(!needs_wrap(&p));
    }

    #[test]
    fn env_withholding_a_secret_forces_a_wrap() {
        // A scrub that WITHHOLDS a var (relaxed fs/net) must still wrap, so the
        // env-read closure is emitted and the secret can't be recovered cross-process
        // via KERN_PROCARGS2. A passthrough `{env:true}` (nothing withheld) need not.
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.env.enforce = true;
        assert!(
            !needs_wrap(&p),
            "passthrough env withholds nothing → no wrap"
        );
        p.env.withheld = vec!["AWS_SECRET_ACCESS_KEY".to_string()];
        assert!(needs_wrap(&p), "a withheld secret must force the SBPL wrap");
        assert!(
            !needs_sandbox(&p),
            "and it is env — not fs/net — driving it"
        );
    }

    #[test]
    fn every_wrapped_profile_carries_the_env_read_closure() {
        // The closure is unconditional: any wrapped profile (here: an fs-confining one)
        // denies process-info* for all-but-self, and NEVER re-grants the same-sandbox
        // form the base once carried (the env-leak footgun).
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec(), None);
        assert!(prof.contains("(deny process-info*)\n"));
        assert!(prof.contains("(allow process-info* (target self))"));
        assert!(
            !prof.contains("(allow process-info* (target same-sandbox))"),
            "the same-sandbox process-info grant re-opens the env-read hole"
        );
        assert!(
            !prof.contains("(allow process-info* (target others))"),
            "target-others leaks a sibling's env"
        );
        // The sysctl arm stays shut by deny-default: no broad kern. prefix and no
        // procargs sysctl is ever allowed (either would re-admit the procargs2 read).
        assert!(!prof.contains("(sysctl-name-prefix \"kern.\")"));
        assert!(!prof.contains("kern.procargs"));
    }
}
