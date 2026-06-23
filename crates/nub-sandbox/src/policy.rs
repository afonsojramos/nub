//! The unified sandbox policy model — `SandboxPolicy { env, fs, net, pid }`.
//!
//! This is the single schema that drives BOTH the build-jail profile (wired to
//! dependency lifecycle scripts, default-ON) and — later — the runtime profile
//! (`nub run` / `nub <file>` / `nubx`). The build-jail is the FIRST consumer;
//! it is the [`build_jail`](crate::build_jail) preset applied to a lifecycle
//! spawn. No build-jail value lives outside this schema (see `.fray/sandbox.md`
//! "engine architecture" + `.fray/sandbox.findings/api.md`).
//!
//! The value forms mirror the config surface: a missing axis means "inherit /
//! no enforcement"; an explicit policy is deny-by-default for env/fs-write/net
//! once present. The build-jail preset always supplies an explicit policy on
//! every axis (it is default-ON, not opt-in-by-presence).

use std::path::PathBuf;

/// Top-level sandbox policy. One per spawned process; every axis composes
/// independently. Built by a profile preset (today: [`crate::build_jail`]).
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
    pub deny_substring: Vec<String>,
    /// `true` once the allowlist is active (deny-by-default). `false` = no env
    /// confinement (the absent-policy / runtime-default case).
    pub enforce: bool,
}

impl EnvPolicy {
    /// Decide whether an inherited env key survives the scrub. Mirrors aube's
    /// `safe_jail_env_key` but driven by data so the build-jail and runtime
    /// profiles share one decision function.
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
        if self.allow_exact.iter().any(|k| k == key) {
            return true;
        }
        self.allow_prefix.iter().any(|p| key.starts_with(p))
    }
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
    /// deny set + globs apply). The build-jail uses generous-read + deny-set.
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

/// `*.example.com` matches `a.example.com` AND `example.com`; an exact pattern
/// matches only itself (case-insensitive — DNS is case-insensitive).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
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
    fn env_no_enforce_admits_everything() {
        let p = EnvPolicy::default();
        assert!(p.admits("ANYTHING_AT_ALL"));
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
