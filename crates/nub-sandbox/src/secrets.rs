//! The zero-breakage default deny-sets, grounded in the §8.5 attack→capability
//! mapping (`.fray/build-jail-design.md`) and SRT's `macGetMandatoryDenyPatterns`.
//!
//! These are the paths/patterns NO legitimate build reads or writes (verified by
//! the §4 `strace` footprint of a 6-package native-build corpus). Denying them is
//! pure upside — it spends no breakage budget while closing the credential-theft
//! and persistence channels of every install-time attack in the canon.

use std::path::{Path, PathBuf};

/// Secret-bearing paths to DENY-READ, resolved under the given `home`. Covers:
/// classic creds (`~/.ssh`, `~/.aws`, `~/.npmrc`-class), VCS/cloud tokens,
/// the 2024–26 crypto wave (wallet/keystore + browser-extension profiles), and
/// the macOS Keychain. The HOME-repoint already neutralizes `~/.npmrc` /
/// `~/.gitconfig`, so those need no explicit entry.
///
/// Note: in the build-jail the child's HOME is repointed at a throwaway
/// jail-home, so `~`-relative secrets resolve to an empty dir anyway. These
/// rules deny the REAL home too (defense-in-depth) — a script that hardcodes an
/// absolute `/Users/<me>/.ssh` still can't read it.
pub fn read_deny_paths(home: &Path) -> Vec<PathBuf> {
    let h = |rel: &str| home.join(rel);
    let mut v = vec![
        // classic credentials
        h(".ssh"),
        h(".aws"),
        h(".netrc"),
        h(".git-credentials"),
        h(".docker/config.json"),
        h(".kube"),
        h(".config/gcloud"),
        h(".config/gh"),
        h(".config/hub"),
        h(".npmrc"), // belt-and-suspenders beyond the HOME repoint
        // crypto wallets / keystores (BeaverTail, TrapDoor — the 2024-26 wave)
        h(".config/solana"),
        h(".config/sui"),
        h(".aptos"),
        h(".electrum"),
        h(".ethereum/keystore"),
        h(".bitcoin"),
    ];
    // macOS Keychain (also needs securityd mach-lookup the profile withholds —
    // second layer). Harmless on other OSes (the path just won't exist).
    v.push(h("Library/Keychains"));
    // browser profile/cookie dirs — wallet-extension profiles + session cookies.
    for p in BROWSER_PROFILE_RELDIRS {
        v.push(h(p));
    }
    v
}

/// Browser profile directories holding cookies + wallet-extension state. Listed
/// for every OS layout; non-existent ones are simply skipped at rule-apply time.
const BROWSER_PROFILE_RELDIRS: &[&str] = &[
    // macOS
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Firefox",
    "Library/Application Support/Microsoft Edge",
    // Linux
    ".config/google-chrome",
    ".config/BraveSoftware",
    ".mozilla/firefox",
    ".config/microsoft-edge",
];

/// Glob patterns DENIED at read at every depth under a read-allow root. The
/// recursive `.env*` carve-out (`.fray/sandbox-fs-deny-list.md`): legitimate
/// code reads secrets via the injected process env, not by `fs.read()`-ing the
/// file, so this is near-zero-breakage and closes a real exfil path.
pub fn read_deny_globs() -> Vec<String> {
    vec![
        "**/.env".into(),
        "**/.env.*".into(),
        // also catch a top-level (non-nested) match
        ".env".into(),
        ".env.*".into(),
    ]
}

/// Relative paths DENIED-WRITE even inside an otherwise-writable root —
/// persistence/backdoor sinks (Shai-Hulud v2, nx). The build-jail's write set
/// is already tight (package dir + caches), but when a writable root overlaps a
/// project tree (the `.git/hooks` carve-out, future runtime profile) these must
/// stay denied so a write can't drop persistence. Matched as suffix/glob
/// against a candidate write path.
pub fn write_deny_relglobs() -> Vec<String> {
    vec![
        // OS / shell persistence
        "**/.bashrc".into(),
        "**/.zshrc".into(),
        "**/.profile".into(),
        "**/.config/systemd/**".into(),
        // CI / workflow injection
        "**/.github/workflows/**".into(),
        // IDE / agent config (agent-aware attacks)
        "**/.vscode/**".into(),
        "**/.claude/**".into(),
        "**/.cursor/**".into(),
        // git internals beyond the narrow hooks carve-out
        "**/.git/config".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn read_deny_includes_ssh_aws_and_wallets() {
        let home = Path::new("/home/user");
        let deny = read_deny_paths(home);
        assert!(deny.contains(&home.join(".ssh")));
        assert!(deny.contains(&home.join(".aws")));
        assert!(deny.contains(&home.join(".config/solana")));
        assert!(deny.iter().any(|p| p.ends_with("Library/Keychains")));
    }

    #[test]
    fn env_globs_cover_nested_dotenv() {
        let g = read_deny_globs();
        assert!(g.iter().any(|p| p.contains(".env")));
        // a monorepo packages/api/.env should be caught by **/.env
        assert!(g.contains(&"**/.env".to_string()));
    }
}
