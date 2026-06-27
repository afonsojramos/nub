//! The default egress allowlist — DATA, overridable as data (never a boolean
//! per host). From `.fray/script-sandbox-design.md` §3 + §8.5 refinement #4,
//! reshaped 2026-06-26 by the prefetch-primary decision (`.fray/build-jail-default-on.md`).
//!
//! The default net posture is **deny-all** (the engine already enforces coarse
//! net-deny; this list is the graceful-degradation fallback for a prefetch miss,
//! kept DOWNLOAD-ONLY). Two distinctions drive what may live here:
//!   - a **writable** host (an attacker can PUT/POST to it) is an EXFIL SINK and
//!     is NEVER a default — removed 2026-06-26: `api.github.com` (attacker PAT →
//!     `POST /gists`) and `*.s3.amazonaws.com` (attacker's own creds → token
//!     scrub gives zero protection). These move to per-package grants only.
//!   - a **download-only** host can serve attacker content but can't be an exfil
//!     sink; any second stage it serves still runs INSIDE the jail. Tolerable as
//!     a fallback — but under prefetch-primary nub fetches the artifact OUTSIDE
//!     the jail, so even these (the github-releases block below) are slated to
//!     drop once prefetch coverage lands. See `.fray/build-jail-default-on.md`.
//!
//! Still excludes the `github.com` apex / `*.github.io` (TrapDoor exfils to
//! `*.github.io` Gists) and `raw.githubusercontent.com` (arbitrary repo content).

/// Hosts native/prebuilt builds need with zero per-package configuration. The
/// registry host(s) are added by the caller from `.npmrc` (so a corporate
/// Artifactory works), not hard-coded here.
///
/// DOWNLOAD-ONLY fallback only — no writable/exfil-sink host appears here (see
/// the module doc). Slated to shrink toward empty as prefetch coverage lands.
pub fn default_allow_hosts() -> Vec<String> {
    [
        // node-gyp Node headers / SHASUMS / win node.lib (default disturl).
        // Vendor single-tenant, download-only.
        "nodejs.org",
        "*.nodejs.org",
        // GitHub release ASSETS only — download-only (GET; cannot be an exfil
        // sink). Deliberately NOT `*.githubusercontent.com` (admits
        // `raw.githubusercontent.com` = arbitrary repo content), NOT the
        // github.com apex, NOT *.github.io. Multi-tenant, so an attacker CAN
        // serve a release asset here — tolerated only because it's download-only
        // and prefetch-primary will remove the need for it entirely.
        "objects.githubusercontent.com",
        // git-archive / tarball fetches for github: deps (separate host).
        // Download-only, same prefetch-removal note as above.
        "codeload.github.com",
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
