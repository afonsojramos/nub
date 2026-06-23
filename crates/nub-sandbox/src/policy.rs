//! The unified sandbox policy model — `SandboxPolicy { env, fs, net, pid }`.
//!
//! This is the single schema that drives BOTH the script-sandbox profile (wired to
//! dependency lifecycle scripts, default-ON) and — later — the runtime profile
//! (`nub run` / `nub <file>` / `nubx`). The script-sandbox is the FIRST consumer;
//! it is the [`script_sandbox`](crate::script_sandbox) preset applied to a lifecycle
//! spawn. No script-sandbox value lives outside this schema (see `.fray/sandbox.md`
//! "engine architecture" + `.fray/sandbox.findings/api.md`).
//!
//! The value forms mirror the config surface: a missing axis means "inherit /
//! no enforcement"; an explicit policy is deny-by-default for env/fs-write/net
//! once present. The script-sandbox preset always supplies an explicit policy on
//! every axis (it is default-ON, not opt-in-by-presence).

use std::path::PathBuf;

/// Top-level sandbox policy. One per spawned process; every axis composes
/// independently. Built by a profile preset (today: [`crate::script_sandbox`]).
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    pub env: EnvPolicy,
    pub fs: FsPolicy,
    pub net: NetPolicy,
    pub pid: PidPolicy,
}

/// Environment-variable confinement, enforced on the spawn-boundary env
/// rebuild (NOT an OS primitive — env is filtered in-process before exec).
///
/// Deny-by-default: when a policy is present the child env is cleared and ONLY
/// the allowlisted keys (plus nub's own required plumbing the caller injects)
/// are re-admitted; `deny` then subtracts from that set (a deny always wins).
/// This is the §8.5 "known-safe allowlist passes through; virtually nothing
/// else" posture — no `*_TOKEN`/`*_KEY`/`AWS_*`/`NPM_TOKEN`/credentials.
#[derive(Debug, Clone, Default)]
pub struct EnvPolicy {
    /// Exact keys admitted verbatim.
    pub allow_exact: Vec<String>,
    /// Key prefixes admitted (e.g. `npm_config_`). A key matching any prefix
    /// passes unless a `deny_substring` rejects it.
    pub allow_prefix: Vec<String>,
    /// Substrings that reject a key even if it matched an allow rule
    /// (`token`/`auth`/`password`/`credential`/`secret`/`key`). Case-insensitive.
    /// Use only for substrings long enough to be unambiguous — a short one like
    /// `pat` would false-positive on `compatible`, so those go in `deny_token`.
    pub deny_substring: Vec<String>,
    /// Short secret markers matched as a WHOLE underscore/boundary-delimited
    /// token, not a raw substring — so `pat` rejects `GH_PAT`/`MY_PAT` but NOT
    /// `compatible`, and `pwd` rejects `DB_PWD` but not `cwd`-containing keys.
    /// Case-insensitive. (HIGH-2: blunt substring matching over-denies for short
    /// markers; anchored matching closes the secret class without breakage.)
    pub deny_token: Vec<String>,
    /// `true` once the allowlist is active (deny-by-default). `false` = no env
    /// confinement (the absent-policy / runtime-default case).
    pub enforce: bool,
}

impl EnvPolicy {
    /// Decide whether an inherited env key survives the scrub. Mirrors aube's
    /// the safe-env-key allowlist but driven by data so the script-sandbox and runtime
    /// profiles share one decision function. A deny (substring OR token) is
    /// checked FIRST and wins over any allow — so a secret riding an allow
    /// prefix (`npm_config_<scope>:_authToken`) is still rejected.
    pub fn admits(&self, key: &str) -> bool {
        if !self.enforce {
            return true;
        }
        let lower = key.to_ascii_lowercase();
        if self
            .deny_substring
            .iter()
            .any(|d| lower.contains(&d.to_ascii_lowercase()))
        {
            return false;
        }
        if self
            .deny_token
            .iter()
            .any(|t| key_contains_token(&lower, &t.to_ascii_lowercase()))
        {
            return false;
        }
        if self.allow_exact.iter().any(|k| k == key) {
            return true;
        }
        self.allow_prefix.iter().any(|p| key.starts_with(p))
    }
}

/// True when `token` appears in `key` as a whole boundary-delimited segment.
/// Boundaries are the start/end of the string or a non-alphanumeric char
/// (`_`/`-`/`.`/`/`/`:`). So `pat` matches `gh_pat`, `pat`, `x.pat` — but not
/// `compatible` or `path`. Both args must already be lowercased.
fn key_contains_token(key: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = key.as_bytes();
    let is_boundary = |b: u8| !b.is_ascii_alphanumeric();
    let mut start = 0;
    while let Some(pos) = key[start..].find(token) {
        let i = start + pos;
        let before_ok = i == 0 || is_boundary(bytes[i - 1]);
        let after = i + token.len();
        let after_ok = after == bytes.len() || is_boundary(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
    }
    false
}

/// Filesystem confinement. Read is generous-allow + deny-the-secret-set
/// (defense-in-depth behind the net gate); write is tight allow-only.
#[derive(Debug, Clone, Default)]
pub struct FsPolicy {
    /// Read-allow roots (subtree grants). Empty = no read restriction.
    pub read_allow: Vec<PathBuf>,
    /// Read-deny paths/subtrees — the secret set (wins over read_allow).
    /// Mirrors SRT's mandatory-deny: `~/.ssh`, `~/.aws`, wallet keystores,
    /// IDE/agent config, etc. See [`crate::secrets`].
    pub read_deny: Vec<PathBuf>,
    /// Deny-read glob patterns matched at ANY depth under a read-allow root
    /// (e.g. `.env`, `.env.*`). The recursive `.env*` carve-out from
    /// `.fray/sandbox-fs-deny-list.md`.
    pub read_deny_glob: Vec<String>,
    /// Write-allow roots (subtree grants). When a write policy is present
    /// everything else is read-only. Empty with `write_enforce` = read-only fs.
    pub write_allow: Vec<PathBuf>,
    /// `true` once the read-allow set is the authoritative allowlist (Landlock
    /// curated read-set / Seatbelt re-allow). `false` = generous read (only the
    /// deny set + globs apply). The script-sandbox uses generous-read + deny-set.
    pub read_enforce: bool,
    /// `true` once write is confined to `write_allow`.
    pub write_enforce: bool,
}

/// Network egress confinement — a per-host allowlist enforced by the localhost
/// egress proxy (§3) plus an OS-level deny-direct-egress backstop. The list is
/// DATA (overridable as data), never a boolean per host.
#[derive(Debug, Clone, Default)]
pub struct NetPolicy {
    /// Egress host allowlist. Supports `host` and `*.host` wildcards. Empty
    /// with `enforce` = network OFF (deny-all egress).
    pub allow_hosts: Vec<String>,
    /// Subtractive host denylist (wins over allow).
    pub deny_hosts: Vec<String>,
    /// `true` = egress is confined to `allow_hosts` (via proxy + OS deny).
    /// `false` = no network confinement.
    pub enforce: bool,
}

impl NetPolicy {
    /// Whether egress to `host` is permitted. A deny always wins; otherwise the
    /// host must match an allow entry. With `enforce == false`, all egress is
    /// allowed.
    pub fn permits_host(&self, host: &str) -> bool {
        if !self.enforce {
            return true;
        }
        if self.deny_hosts.iter().any(|h| host_matches(h, host)) {
            return false;
        }
        self.allow_hosts.iter().any(|h| host_matches(h, host))
    }
}

/// Normalize a host for matching: lowercase (DNS is case-insensitive) and strip
/// a single trailing `.` (an absolute FQDN `github.com.` resolves identically to
/// `github.com` — without this a deny entry is trailing-dot bypassable).
fn normalize_host(h: &str) -> String {
    let h = h.trim().to_ascii_lowercase();
    h.strip_suffix('.').unwrap_or(&h).to_string()
}

/// Match `host` against an allow/deny `pattern`. A leading `*.` is the ONLY
/// wildcard form: `*.example.com` matches `a.example.com`, `a.b.example.com`,
/// AND the apex `example.com`. A `*` in any OTHER position is NOT a wildcard —
/// such a pattern matches NOTHING (fail-closed), so a malformed multi-`*` entry
/// like `*.s3.*.amazonaws.com` does not silently mis-match (enumerate region
/// buckets instead). Exact patterns match only themselves. Both sides are
/// normalized (lowercase + trailing-dot strip).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = normalize_host(host);
    let pattern = normalize_host(pattern);
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // a second `*` past the leading wildcard is unsupported — fail closed
        // rather than match a literal asterisk no real host carries.
        if suffix.contains('*') {
            return false;
        }
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else if pattern.contains('*') {
        // a bare `*` not in leading position is not a wildcard; never matches.
        false
    } else {
        pattern == host
    }
}

/// Process / persistence containment. Kill-the-tree on exit + caps.
#[derive(Debug, Clone)]
pub struct PidPolicy {
    /// Active-process cap (Job Object active-process / `RLIMIT_NPROC`). `None`
    /// = uncapped.
    pub max_processes: Option<u32>,
    /// Kill the spawned process tree when the parent drops/exits. The
    /// kill-on-close primitive (Job Object on Windows; process-group kill +
    /// `kill_on_drop` on Unix).
    pub kill_on_exit: bool,
}

impl Default for PidPolicy {
    fn default() -> Self {
        // Default-safe: reap the tree (matches today's aube `kill_on_drop`),
        // no process cap until a profile sets one.
        Self {
            max_processes: None,
            kill_on_exit: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_allowlist_denies_secrets_even_when_prefix_allowed() {
        let p = EnvPolicy {
            allow_exact: vec!["PATH".into()],
            allow_prefix: vec!["npm_config_".into()],
            deny_substring: vec!["token".into(), "secret".into()],
            deny_token: vec!["pat".into(), "pwd".into()],
            enforce: true,
        };
        assert!(p.admits("PATH"));
        assert!(p.admits("npm_config_registry"));
        // a token-bearing npm_config_ key is rejected by the deny substring
        assert!(!p.admits("npm_config_//registry.npmjs.org/:_authToken"));
        assert!(!p.admits("AWS_SECRET_ACCESS_KEY"));
        assert!(!p.admits("GITHUB_TOKEN"));
        // unknown non-secret key is still denied (deny-by-default)
        assert!(!p.admits("RANDOM_VAR"));
    }

    #[test]
    fn env_deny_token_is_anchored_not_substring() {
        let p = EnvPolicy {
            allow_exact: vec!["COMPATIBLE_MODE".into(), "CWD_HINT".into()],
            allow_prefix: vec![],
            deny_substring: vec![],
            deny_token: vec!["pat".into(), "pwd".into()],
            enforce: true,
        };
        // anchored token rejects GH_PAT / DB_PWD ...
        assert!(!p.admits("GH_PAT"));
        assert!(!p.admits("DB_PWD"));
        assert!(!p.admits("PAT"));
        // ... but does NOT false-positive on substrings of longer words
        assert!(p.admits("COMPATIBLE_MODE")); // contains "pat" mid-word
        assert!(p.admits("CWD_HINT")); // contains "pwd"? no — "cwd"; sanity
    }

    #[test]
    fn env_no_enforce_admits_everything() {
        let p = EnvPolicy::default();
        assert!(p.admits("ANYTHING_AT_ALL"));
    }

    #[test]
    fn host_trailing_dot_and_bad_wildcard_are_normalized() {
        // trailing-dot FQDN must match (and be deny-able)
        assert!(host_matches("github.com", "github.com."));
        assert!(host_matches("*.example.com", "a.example.com."));
        // a non-leading wildcard matches nothing (fail-closed)
        assert!(!host_matches(
            "*.s3.*.amazonaws.com",
            "x.s3.us-east-1.amazonaws.com"
        ));
        assert!(!host_matches("evil*.com", "evilfoo.com"));
        // *.foo.com must NOT match foo.com.attacker.net or evil-foo.com
        assert!(!host_matches("*.foo.com", "foo.com.attacker.net"));
        assert!(!host_matches("*.foo.com", "evil-foo.com"));
    }

    #[test]
    fn net_host_wildcard_and_apex() {
        let p = NetPolicy {
            allow_hosts: vec!["*.githubusercontent.com".into(), "nodejs.org".into()],
            deny_hosts: vec![],
            enforce: true,
        };
        assert!(p.permits_host("objects.githubusercontent.com"));
        assert!(p.permits_host("githubusercontent.com")); // apex via *.
        assert!(p.permits_host("nodejs.org"));
        assert!(!p.permits_host("evil.com"));
        // the TrapDoor lesson: github.com apex is NOT implied by any entry
        assert!(!p.permits_host("github.com"));
    }

    #[test]
    fn net_deny_wins_over_allow() {
        let p = NetPolicy {
            allow_hosts: vec!["*.example.com".into()],
            deny_hosts: vec!["bad.example.com".into()],
            enforce: true,
        };
        assert!(p.permits_host("good.example.com"));
        assert!(!p.permits_host("bad.example.com"));
    }
}
