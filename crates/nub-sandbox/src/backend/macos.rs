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
//!   - net:    not-enforced → `(allow network*)`; enforced → the base deny stands
//!     (coarse deny). Per-host is the egress proxy's job (S6) via [`PROXY_PORT`].
//!   - env:    construction, not an SBPL primitive — the child env IS the policy's
//!     constructed map (handled here when wrapping, mirrored from the skeleton).
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

/// Loopback egress-proxy port. `None` until the proxy lands (S6); PER-HOST external
/// egress then has no enforcement point, so an enforced net with allow-rules is
/// reported degraded (coarse deny). Loopback itself is always carved out (below).
const PROXY_PORT: Option<u16> = None;

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
pub fn apply(policy: &SandboxPolicy, spec: CommandSpec) -> Result<Prepared, Degradation> {
    if !needs_sandbox(policy) {
        // No fs/net confinement — just the env-scrub (or nothing).
        return Ok(Prepared {
            command: base_command(&spec, policy),
            degradation: Degradation::full(),
        });
    }

    let profile = build_profile(policy, &spec);
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

    Ok(Prepared {
        command: wrapped,
        degradation: degradation(policy),
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

/// A profile is emitted only when there is an fs or net axis to enforce. A fully
/// relaxed fs + non-enforcing net needs no kernel confinement.
fn needs_sandbox(policy: &SandboxPolicy) -> bool {
    fs_confines(policy) || policy.net.enforce
}

/// Whether the fs axis confines anything. A relaxed axis is `default_effect ==
/// Allow` with no entries (allow everything); anything else confines.
fn fs_confines(policy: &SandboxPolicy) -> bool {
    policy.fs.rules.default_effect != Effect::Allow || !policy.fs.rules.entries.is_empty()
}

/// Build the full SBPL profile text for `policy`.
fn build_profile(policy: &SandboxPolicy, spec: &CommandSpec) -> String {
    let mut out = String::with_capacity(MACOS_SEATBELT_BASE.len() + 2048);
    out.push_str(MACOS_SEATBELT_BASE);
    out.push('\n');

    emit_net(policy, &mut out);
    emit_fs(policy, spec, &mut out);

    out
}

/// Net axis. Not-enforced → allow all egress + the DNS/TLS service block (we only
/// wrapped for fs). Enforced → the base `(deny default)` denies external egress;
/// loopback is carved out unconditionally (local IPC + the future egress proxy live
/// there), while per-host EXTERNAL allows await the proxy (S6) — see [`degradation`].
fn emit_net(policy: &SandboxPolicy, out: &mut String) {
    if !policy.net.enforce {
        out.push_str("(allow network*)\n");
        out.push_str(NETWORK_SERVICES);
        return;
    }
    // Loopback egress only. Seatbelt requires `*`/`localhost` as the host in a
    // `remote ip` literal — a `127.0.0.1` literal is a PARSE ERROR that fails the
    // whole profile load. `localhost` covers loopback on both 127.0.0.1 and ::1;
    // `:*` admits any port, so the egress proxy (whatever port it binds) is reachable.
    out.push_str("(allow network* (remote ip \"localhost:*\"))\n");
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
}

/// Top-level roots a write grant must never cover — a `..`-collapsed surface path
/// (`/tmp/..` → `/private`) would otherwise open filesystem-wide write. Reads are
/// exempt (a generous `(subpath "/")` read is the legitimate default posture).
fn is_dangerous_write_root(term: &MatchTerm) -> bool {
    let MatchTerm::Subpath(p) = term else {
        return false;
    };
    matches!(
        p.as_str(),
        "/" | "/private"
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

/// Full-enforcement unless net enforces per-host allows the coarse deny can't honor
/// (no proxy yet) — then report `net-per-host` degraded (fail-safe, not silent).
fn degradation(policy: &SandboxPolicy) -> Degradation {
    let mut deg = Degradation::full();
    if policy.net.enforce
        && PROXY_PORT.is_none()
        && policy.net.rules.iter().any(|r| r.effect == Effect::Allow)
    {
        deg.lost.push("net-per-host".to_string());
        deg.reason = Some(
            "egress proxy not wired — per-host allows denied (coarse network deny)".to_string(),
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
        let prof = build_profile(&p, &spec());
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
        let prof = build_profile(&p, &spec());
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
        let prof = build_profile(&p, &spec());
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
        let prof = build_profile(&p, &spec());
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
    fn confstr_grants_temp_not_cache() {
        // Only the DARWIN TEMP dir is granted; the persistent CACHE dir (…/C) is a
        // cross-build poisoning surface and must NOT be write-granted.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec());
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
        let prof = build_profile(&p, &spec());
        assert!(!prof.contains("(allow file-write* (subpath \"/private\"))"));
        assert!(is_dangerous_write_root(&MatchTerm::Subpath(
            "/private".to_string()
        )));
        assert!(!is_dangerous_write_root(&MatchTerm::Subpath(
            "/proj".to_string()
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
        let prof = build_profile(&p, &spec());
        assert!(prof.contains("(allow file*)"));
    }

    #[test]
    fn net_enforced_carves_loopback_only() {
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![],
            default_effect: Effect::Deny,
        };
        let prof = build_profile(&p, &spec());
        assert!(prof.contains("(allow network* (remote ip \"localhost:*\"))"));
        // No blanket network allow when enforcing.
        assert!(!prof.contains("(allow network*)\n"));
    }

    #[test]
    fn net_not_enforced_allows_all_plus_services() {
        // fs confines (so we wrap) but net is relaxed → full egress + DNS/TLS block.
        let p = fs_policy(
            Effect::Deny,
            vec![rule("/proj", Effect::Allow, FsAccess::ReadWrite)],
        );
        let prof = build_profile(&p, &spec());
        assert!(prof.contains("(allow network*)\n"));
        assert!(prof.contains("com.apple.trustd"));
    }

    #[test]
    fn degradation_reports_lost_per_host_without_proxy() {
        let mut p = fs_policy(Effect::Allow, vec![]);
        p.net = NetPolicy {
            enforce: true,
            rules: vec![NetRule {
                target: NetTarget::Host("example.com".to_string()),
                effect: Effect::Allow,
            }],
            default_effect: Effect::Deny,
        };
        let deg = degradation(&p);
        assert_eq!(deg.lost, vec!["net-per-host".to_string()]);
        // A pure deny-all net (no allow rules) is fully enforced, not degraded.
        p.net.rules.clear();
        assert!(degradation(&p).is_full());
    }

    #[test]
    fn no_sandbox_wrap_when_nothing_confines() {
        // Relaxed fs + non-enforcing net = env-scrub only, no SBPL.
        let p = fs_policy(Effect::Allow, vec![]);
        assert!(!needs_sandbox(&p));
    }
}
