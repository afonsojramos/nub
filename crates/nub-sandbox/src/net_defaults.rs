//! The default egress allowlist — DATA, overridable as data (never a boolean
//! per host). From `.fray/script-sandbox-design.md` §3 + §8.5 refinement #4.
//!
//! The list is deliberately TIGHT: every allowed host is an exfil channel.
//! Crucially it does NOT include the `github.com` apex or `*.github.io`
//! (TrapDoor exfils to `*.github.io` Gists specifically to ride a loose GitHub
//! allowlist) — only the SPECIFIC release-asset hosts.

/// Hosts native/prebuilt builds need with zero per-package configuration. The
/// registry host(s) are added by the caller from `.npmrc` (so a corporate
/// Artifactory works), not hard-coded here.
pub fn default_allow_hosts() -> Vec<String> {
    [
        // node-gyp Node headers / SHASUMS / win node.lib (default disturl)
        "nodejs.org",
        "*.nodejs.org",
        // GitHub release ASSETS only. Deliberately NOT `*.githubusercontent.com`
        // — that wildcard admits `raw.githubusercontent.com`, which serves
        // arbitrary user-controlled repo content (an exfil-read / payload-fetch
        // channel; the TrapDoor lesson generalizes past `*.github.io`). Release
        // assets 302 to `objects.githubusercontent.com` specifically, so name it
        // exactly. NOT the github.com apex, NOT *.github.io.
        "objects.githubusercontent.com",
        // prebuild-install --token resolves the asset id first
        "api.github.com",
        // git-archive / tarball fetches for github: deps (separate host)
        "codeload.github.com",
        // node-pre-gyp's common (not universal) binary.host region buckets.
        // `*.` matches only one wildcard segment, so the regional form
        // (`mapbox-node-binary.s3.us-east-1.amazonaws.com`) needs its own
        // entry — a `*.s3.*.amazonaws.com` double-wildcard matches NOTHING in
        // this matcher (fail-closed). Region buckets that don't fit either
        // global form go through the `sandbox-allow-hosts` per-project override.
        "*.s3.amazonaws.com",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Opt-in browser/driver/engine CDN hosts (§3 "plus the per-package-opt-in"
/// block). Bundled into the default list only when
/// [`ScriptSandboxParams::bundle_browser_cdns`](crate::ScriptSandboxParams) is set
/// (§9(d) maintainer-owned). Widens the exfil surface by ~7 hosts, so off by
/// default in the engine; the embedder flips it per the maintainer's call.
pub fn browser_cdn_hosts() -> Vec<String> {
    [
        "storage.googleapis.com",     // puppeteer Chrome, chromedriver
        "googlechromelabs.github.io", // puppeteer + chromedriver version JSON
        "binaries.prisma.sh",         // prisma engines
        "downloads.sentry-cdn.com",   // @sentry/cli
        "archive.mozilla.org",        // puppeteer -> firefox
        "product-details.mozilla.org",
        "download.cypress.io", // cypress -> 302 -> cdn.cypress.io
        "cdn.cypress.io",      // cypress 302 target
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::host_matches;

    #[test]
    fn default_list_excludes_github_apex_and_pages() {
        let hosts = default_allow_hosts();
        // the load-bearing TrapDoor guard: neither apex nor pages is implied
        assert!(!hosts.iter().any(|h| host_matches(h, "github.com")));
        assert!(!hosts.iter().any(|h| host_matches(h, "attacker.github.io")));
        // but the release-asset hosts ARE allowed
        assert!(
            hosts
                .iter()
                .any(|h| host_matches(h, "objects.githubusercontent.com"))
        );
        assert!(hosts.iter().any(|h| host_matches(h, "codeload.github.com")));
    }
}
