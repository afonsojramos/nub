//! Network host matching: host-glob vs CIDR dispatch and last-match-wins
//! evaluation of a [`NetPolicy`]'s ordered rules.
//!
//! Host wildcard semantics diverge from TLS deliberately: `*.example.com`
//! matches the apex `example.com` AND any-depth subdomain (`a.b.example.com`) —
//! fewer footguns than TLS's single-label wildcard (.fray/sandbox.md matcher spec).

use crate::policy::{Effect, NetPolicy, NetRule, NetTarget};
use std::net::IpAddr;

/// A compiled last-match-wins matcher over a [`NetPolicy`]'s rules.
pub struct HostMatcher<'a> {
    rules: &'a [NetRule],
    default_effect: Effect,
    enforce: bool,
}

impl<'a> HostMatcher<'a> {
    pub fn new(policy: &'a NetPolicy) -> Self {
        Self {
            rules: &policy.rules,
            default_effect: policy.default_effect,
            enforce: policy.enforce,
        }
    }

    /// Whether egress to `host` (a hostname or an IP literal) is admitted. When
    /// the policy does not enforce, everything is admitted. Otherwise the LAST
    /// matching rule wins; nothing matching falls back to `default_effect`.
    pub fn admits(&self, host: &str) -> bool {
        if !self.enforce {
            return true;
        }
        let ip = host.parse::<IpAddr>().ok();
        let mut winner = self.default_effect;
        for rule in self.rules {
            let hit = match &rule.target {
                NetTarget::Host(pat) => host_glob_matches(pat, host),
                NetTarget::Cidr(net) => ip.is_some_and(|ip| net.contains(&ip)),
            };
            if hit {
                winner = rule.effect;
            }
        }
        matches!(winner, Effect::Allow)
    }
}

/// Match a host pattern against a concrete host. Two forms:
///   `*.example.com` → apex + any-depth subdomain (case-insensitive);
///   a literal `example.com` → exact (case-insensitive).
/// A bare `*` matches any host.
pub fn host_glob_matches(pattern: &str, host: &str) -> bool {
    let pat = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if pat == "*" {
        return true;
    }
    if let Some(suffix) = pat.strip_prefix("*.") {
        // apex match, or any-depth subdomain (must end with `.<suffix>`).
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    pat == host
}
