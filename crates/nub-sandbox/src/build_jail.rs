//! The build-jail PROFILE — the first consumer of the sandbox engine.
//!
//! Assembles a [`SandboxPolicy`] from the default-ON build-jail defaults
//! (`.fray/build-jail-design.md` §3/§4/§5/§8.5), parameterized by the spawn's
//! package dir, project root, resolved HOME/cache dirs, and the configured
//! registry hosts. This is the drop-in replacement for today's aube install
//! jail, at parity (env-scrub + Landlock/Seatbelt fs + coarse net) and BEYOND
//! it on the secret-deny set and the per-host egress allowlist.

use crate::net_defaults;
use crate::policy::{EnvPolicy, FsPolicy, NetPolicy, PidPolicy, SandboxPolicy};
use crate::secrets;
use std::path::{Path, PathBuf};

/// Inputs needed to build the per-spawn build-jail policy. Supplied by the
/// embedder (nub) at the install-lifecycle seam.
#[derive(Debug, Clone)]
pub struct BuildJailParams {
    /// The dependency package dir whose lifecycle script is running (read+write).
    pub package_dir: PathBuf,
    /// The project root (read; never write — project SOURCE is read-only).
    pub project_root: PathBuf,
    /// The per-package throwaway jail-home (read+write; HOME/TMP repoint target).
    pub jail_home: PathBuf,
    /// The real user home — used to resolve the secret deny-set absolute paths.
    pub user_home: PathBuf,
    /// Extra writable roots the build legitimately needs: `~/.cache/node-gyp`,
    /// `~/.npm/_prebuilds`, the nub-owned shared build cache, etc. (§5).
    pub extra_write: Vec<PathBuf>,
    /// Configured registry host(s) from `.npmrc` (`registry=` + scoped). Added
    /// to the egress allowlist so a corporate Artifactory works.
    pub registry_hosts: Vec<String>,
    /// Additional egress hosts from `jail-allow-hosts` (per-project / per-package
    /// override — node-pre-gyp custom S3 buckets, mirrors).
    pub extra_hosts: Vec<String>,
    /// Whether to bundle the opt-in browser/driver CDN hosts (puppeteer, cypress,
    /// prisma, sentry). §9(d) — maintainer-owned default; the preset honors it.
    pub bundle_browser_cdns: bool,
}

/// The default substrings that reject an env key regardless of allowlist match.
/// Superset of aube's (`token`/`auth`/`password`/`credential`/`secret`) plus
/// `key` — the §8.5 "no `*_KEY`" rule (AWS_SECRET_ACCESS_KEY, *_API_KEY).
fn default_env_deny_substrings() -> Vec<String> {
    vec![
        "token".into(),
        "auth".into(),
        "password".into(),
        "passwd".into(),
        "credential".into(),
        "secret".into(),
        "key".into(),
        "session".into(),
    ]
}

/// The known-safe env allowlist — the minimal set a build needs (§8.5). Mirrors
/// aube's `safe_jail_env_key` exact list; the `npm_config_` prefix is admitted
/// (minus any key the deny-substrings reject, e.g. `npm_config_..._authToken`).
fn build_jail_env() -> EnvPolicy {
    EnvPolicy {
        allow_exact: [
            // process basics a build cannot run without
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "TERM",
            "LANG",
            "LC_ALL",
            "INIT_CWD",
            // npm lifecycle plumbing
            "npm_lifecycle_event",
            "npm_lifecycle_script",
            "npm_package_name",
            "npm_package_version",
            "npm_package_json",
            "npm_command",
            "npm_node_execpath",
            "npm_execpath",
            "NODE",
            // egress-proxy plumbing (the §3 vars must survive the scrub)
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
            "GIT_SSH_COMMAND",
            "GRPC_PROXY",
            // the big-downloader cache redirects (§5(4)) — values point at the
            // nub-owned shared build cache, which is in the write set
            "PUPPETEER_CACHE_DIR",
            "CYPRESS_CACHE_FOLDER",
            "ELECTRON_CACHE",
            "electron_config_cache",
            "PRISMA_ENGINES_CACHE_DIR",
            "GECKODRIVER_CACHE_DIR",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        // npm_config_* + npm_package_* (manifest fields) pass; NODE_OPTIONS is
        // injected by the embedder overlay, not inherited.
        allow_prefix: vec!["npm_config_".into(), "npm_package_".into()],
        deny_substring: default_env_deny_substrings(),
        enforce: true,
    }
}

/// FS policy: generous-read + deny-the-secret-set (defense-in-depth, net gate
/// primary), write confined to package dir + jail-home + caches (§4/§5).
fn build_jail_fs(p: &BuildJailParams) -> FsPolicy {
    let mut write_allow = vec![p.package_dir.clone(), p.jail_home.clone()];
    write_allow.extend(p.extra_write.iter().cloned());

    let mut read_deny = secrets::read_deny_paths(&p.user_home);
    // The jail-home is the only HOME the child sees, but if the build resolves
    // an absolute path into the real home's secret dirs, deny it (above). Also
    // deny the user's real .env under the project (covered by globs too).
    read_deny.dedup();

    FsPolicy {
        // generous read (read_enforce=false) — only the deny set + globs apply,
        // matching aube's broad `/` read grant minus the secrets. This is the
        // §4 verdict (project-dir-only breaks 100% of builds).
        read_allow: vec![p.project_root.clone()],
        read_deny,
        read_deny_glob: secrets::read_deny_globs(),
        write_allow,
        read_enforce: false,
        write_enforce: true,
    }
}

/// Net policy: the tight default egress allowlist (§3, §8.5 refinement #4 — no
/// `github.com` apex). Always `enforce` — the build-jail confines egress to the
/// proxy + the allowed hosts.
fn build_jail_net(p: &BuildJailParams) -> NetPolicy {
    let mut allow_hosts = net_defaults::default_allow_hosts();
    allow_hosts.extend(p.registry_hosts.iter().cloned());
    allow_hosts.extend(p.extra_hosts.iter().cloned());
    if p.bundle_browser_cdns {
        allow_hosts.extend(net_defaults::browser_cdn_hosts());
    }
    allow_hosts.sort();
    allow_hosts.dedup();
    NetPolicy {
        allow_hosts,
        deny_hosts: vec![],
        enforce: true,
    }
}

/// Build the complete build-jail [`SandboxPolicy`] for one lifecycle spawn.
pub fn policy(p: &BuildJailParams) -> SandboxPolicy {
    SandboxPolicy {
        env: build_jail_env(),
        fs: build_jail_fs(p),
        net: build_jail_net(p),
        pid: PidPolicy {
            // generous active-process cap: native builds fan out cl.exe/cc/make.
            max_processes: Some(512),
            kill_on_exit: true,
        },
    }
}

/// Convenience: derive the canonical extra-write set (the §5 MANDATORY caches)
/// from the resolved home + npm cache dir, so the embedder doesn't re-derive it.
pub fn default_extra_write(user_home: &Path, npm_cache_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut v = vec![
        user_home.join(".cache/node-gyp"),
        user_home.join(".node-gyp"),
        user_home.join(".cache/nub/build-cache"),
    ];
    if let Some(cache) = npm_cache_dir {
        v.push(cache.join("_prebuilds"));
    } else {
        v.push(user_home.join(".npm/_prebuilds"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn params() -> BuildJailParams {
        BuildJailParams {
            package_dir: PathBuf::from("/proj/node_modules/dep"),
            project_root: PathBuf::from("/proj"),
            jail_home: PathBuf::from("/tmp/nub-jail/123/dep-abc"),
            user_home: PathBuf::from("/home/user"),
            extra_write: vec![PathBuf::from("/home/user/.cache/node-gyp")],
            registry_hosts: vec!["registry.npmjs.org".into()],
            extra_hosts: vec![],
            bundle_browser_cdns: false,
        }
    }

    #[test]
    fn build_jail_env_admits_safe_denies_secrets() {
        let env = build_jail_env();
        assert!(env.admits("PATH"));
        assert!(env.admits("npm_config_registry"));
        assert!(env.admits("HTTPS_PROXY"));
        // every secret-shaped var is denied, default-by-default
        assert!(!env.admits("NPM_TOKEN"));
        assert!(!env.admits("AWS_ACCESS_KEY_ID"));
        assert!(!env.admits("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.admits("GITHUB_TOKEN"));
        assert!(!env.admits("STRIPE_SECRET_KEY"));
        assert!(!env.admits("MY_API_KEY"));
        // a random non-secret var the build didn't declare: still denied
        assert!(!env.admits("FOO_BAR"));
    }

    #[test]
    fn build_jail_net_is_tight_no_github_apex() {
        let net = build_jail_net(&params());
        assert!(net.permits_host("registry.npmjs.org"));
        assert!(net.permits_host("nodejs.org"));
        assert!(net.permits_host("objects.githubusercontent.com"));
        // §8.5 #4: github.com apex is NOT in the default set (TrapDoor abuse).
        assert!(!net.permits_host("github.io"));
        assert!(!net.permits_host("evil-c2.example.com"));
    }

    #[test]
    fn build_jail_fs_write_is_confined_read_denies_secrets() {
        let fs = build_jail_fs(&params());
        assert!(fs.write_enforce);
        assert!(
            fs.write_allow
                .contains(&PathBuf::from("/proj/node_modules/dep"))
        );
        assert!(
            fs.write_allow
                .contains(&PathBuf::from("/home/user/.cache/node-gyp"))
        );
        // project root is readable but NOT writable (source is read-only)
        assert!(!fs.write_allow.contains(&PathBuf::from("/proj")));
        assert!(fs.read_deny.contains(&PathBuf::from("/home/user/.ssh")));
    }

    #[test]
    fn browser_cdns_bundled_only_when_requested() {
        let mut p = params();
        assert!(!build_jail_net(&p).permits_host("storage.googleapis.com"));
        p.bundle_browser_cdns = true;
        assert!(build_jail_net(&p).permits_host("storage.googleapis.com"));
    }
}
