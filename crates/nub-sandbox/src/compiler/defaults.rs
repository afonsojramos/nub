//! The built-in default ENTRIES — the secret deny-set and trusted-host allows
//! that `"..."` spreads into an ordered list at its position. Per .fray/sandbox.md
//! "Built-in defaults are just default ENTRIES, not a floor": these are ordinary
//! last-match-wins entries, so a later user rule can override any of them.
//!
//! The data (secret paths/globs, browser/wallet dirs) is ported verbatim from the
//! reviewed `secrets.rs` in the salvage branches — the §8.5 attack→capability
//! mapping. It is DATA, re-homed under the fresh policy model.

use crate::matcher::path::{Homes, canonicalize_glob_prefix, expand_symbolic};
use crate::policy::{CanonGlob, Effect, FsAccess, FsRule};

/// Secret-bearing paths to DENY-READ, resolved under the home anchors. Classic
/// creds, VCS/cloud tokens, the 2024–26 crypto-wallet wave, browser profiles, and
/// the macOS Keychain. Each becomes a subtree Deny entry (path + `path/**`).
const SECRET_READ_RELPATHS: &[&str] = &[
    // classic credentials
    ".ssh",
    ".aws",
    ".netrc",
    ".git-credentials",
    ".docker/config.json",
    ".kube",
    ".config/gcloud",
    ".config/gh",
    ".config/hub",
    ".npmrc",
    // crypto wallets / keystores
    ".config/solana",
    ".config/sui",
    ".aptos",
    ".electrum",
    ".ethereum/keystore",
    ".bitcoin",
    // macOS Keychain (harmless path elsewhere)
    "Library/Keychains",
    // browser profile/cookie dirs (wallet-extension state + session cookies)
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Firefox",
    "Library/Application Support/Microsoft Edge",
    ".config/google-chrome",
    ".config/BraveSoftware",
    ".mozilla/firefox",
    ".config/microsoft-edge",
];

/// `.env*` deny globs — legit code reads secrets via the injected process env,
/// not by `fs.read()`-ing the file, so denying these is near-zero-breakage.
const SECRET_READ_GLOBS: &[&str] = &["**/.env", "**/.env.*", ".env", ".env.*"];

/// Secret name-word tokens matched as a case-insensitive SUBSTRING anywhere in a
/// key (via [`word_in_substr`]). These are long/specific enough that a substring
/// rule has no realistic false positive, so it catches the forms an exact-segment
/// rule misses — plurals (`SESSION_TOKENS`, `DB_PASSWORDS`, `CREDENTIALS`),
/// undelimited/camelCase (`MYTOKEN`, `myToken`), and fused names
/// (`GOOGLE_APPLICATION_CREDENTIALS`). Ported from the §8.5 env posture.
pub const SECRET_SUBSTR_TOKENS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "apikey",
    "api_key",
];

/// Short/ambiguous secret tokens matched ONLY as a whole `_`/`-`/`.` segment (via
/// [`word_is_segment`]). A substring rule on these would over-match wildly
/// (`pat`→PATH, `auth`→AUTHOR, `pwd`→PWD-as-substring); segment matching still
/// catches `MYSQL_PWD`, `X_AUTH_TOKEN`, `GITHUB_PAT` while sparing PATH/AUTHOR.
/// Bare `PWD` (the CWD var, a whole segment) IS denied under `"..."` — a benign
/// false positive (the working directory is re-derivable). A superstring like
/// `AUTHORIZATION` is NOT caught (segment ≠ `auth`); `["*","..."]` is a
/// best-effort denylist, not a guarantee — real confinement is an allowlist.
pub const SECRET_SEGMENT_TOKENS: &[&str] = &["pat", "pwd", "auth"];

/// Secret-name env prefixes/exact keys denied by default. Matched as
/// case-insensitive globs (a trailing `_` becomes a `*` prefix); no boundary
/// logic needed since these are already anchored names, not bare words.
pub const SECRET_ENV_KEYS: &[&str] = &["AWS_", "NPM_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];

/// Case-insensitive substring test — the match rule for [`SECRET_SUBSTR_TOKENS`].
pub fn word_in_substr(word: &str, key: &str) -> bool {
    key.to_ascii_uppercase()
        .contains(&word.to_ascii_uppercase())
}

/// Whole-segment (case-insensitive) test — the match rule for
/// [`SECRET_SEGMENT_TOKENS`]. The key is split on `_`/`-`/`.` and the word must
/// EQUAL one segment, so `pwd` hits `MYSQL_PWD` but `pat` misses `PATH`.
pub fn word_is_segment(word: &str, key: &str) -> bool {
    let w = word.to_ascii_uppercase();
    segments(key).contains(&w)
}

/// Split a name into non-empty, upper-cased segments on `_`/`-`/`.` boundaries.
fn segments(s: &str) -> Vec<String> {
    s.split(['_', '-', '.'])
        .filter(|seg| !seg.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

/// Build the default fs-read DENY entries (secret paths + `.env*` globs). These
/// are what `"..."` splices into a read ruleset. Deny access is neutral (Read).
pub fn secret_read_denies(homes: &Homes) -> Vec<FsRule> {
    let mut out = Vec::new();
    for rel in SECRET_READ_RELPATHS {
        let anchored = format!("~/{rel}");
        for g in subtree_globs(&expand_symbolic(&anchored, homes)) {
            out.push(deny(g));
        }
    }
    // `.env*` denies are depth-independent (`**/.env` matches any component
    // ending in `.env`), so they are NOT anchored under any root — passed to the
    // matcher verbatim.
    for g in SECRET_READ_GLOBS {
        out.push(deny(g.to_string()));
    }
    out
}

/// The generous read base entry: allow everything, then the secret denies (added
/// by the caller after this) tighten it. Emitted for the wrapper `true` /
/// spread-of-defaults read posture.
pub fn generous_read_allow() -> FsRule {
    FsRule {
        matcher: CanonGlob("**".to_string()),
        effect: Effect::Allow,
        access: FsAccess::Read,
    }
}

/// A subtree grant expands to two globs — the node itself and everything under
/// it — so a bare path like `~/.ssh` denies both `~/.ssh` and `~/.ssh/id_rsa`.
/// A pattern already carrying a glob metachar is emitted as-is (no `/**` suffix).
pub fn subtree_globs(expanded: &str) -> Vec<String> {
    if expanded.contains(['*', '?', '[', '{']) {
        return vec![expanded.to_string()];
    }
    let trimmed = expanded.trim_end_matches('/');
    vec![trimmed.to_string(), format!("{trimmed}/**")]
}

fn deny(glob: String) -> FsRule {
    FsRule {
        matcher: CanonGlob(canonicalize_glob_prefix(&glob)),
        effect: Effect::Deny,
        access: FsAccess::Read,
    }
}

/// Non-secret operational env keys that pass through in the `sandbox: true`
/// curated baseline: PATH + system/locale/toolchain-discovery vars + the
/// build-hint `npm_config_*` subset. Ambient secrets never ride this list. The
/// exact baseline is the deferred build-jail thread's product surface; this is a
/// usable, safe default for the frontend-less engine.
const BASELINE_ENV_EXACT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "TERM",
    "TZ",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
];
const BASELINE_ENV_PREFIXES: &[&str] = &["LC_", "npm_config_"];

/// Build the curated-baseline child env from the ambient env (the `sandbox: true`
/// / build-jail env posture). Only the non-secret operational allowlist passes.
pub fn curated_baseline_env(
    ambient: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    ambient
        .iter()
        .filter(|(k, _)| {
            BASELINE_ENV_EXACT.contains(&k.as_str())
                || BASELINE_ENV_PREFIXES.iter().any(|p| k.starts_with(p))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
