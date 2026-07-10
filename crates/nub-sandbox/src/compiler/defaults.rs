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
    ".gnupg",
    ".aws",
    ".netrc",
    ".git-credentials",
    ".config/git/credentials",
    ".docker/config.json",
    ".kube",
    ".config/gcloud",
    ".config/gh",
    ".config/hub",
    ".npmrc",
    ".pgpass",
    ".pypirc",
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

/// `.env*` / `.envrc` deny globs — legit code reads secrets via the injected
/// process env, not by `fs.read()`-ing the file, so denying these is
/// near-zero-breakage. Both `.env` and `.env.<x>` are denied as a leaf (`**/.env`,
/// `**/.env.*`) AND as a SUBTREE (`**/.env/**`, `**/.env.*/**`) so a `.env/` or
/// `.env.local/` DIRECTORY holding per-target secret files is covered, not just the
/// single-file form. `.envrc` is direnv's secret-bearing shell config, project-local
/// at any depth like `.env`.
///
/// MUST STAY IN SYNC with `linux_grants::BUILTIN_ENV_DENY_GLOBS` (the generous-`**`
/// system-dir seeding skips exactly these depth-independent builtin denies).
const SECRET_READ_GLOBS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/.env/**",
    "**/.env.*/**",
    ".env",
    ".env.*",
    ".env/**",
    ".env.*/**",
    "**/.envrc",
    ".envrc",
];

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
        access: FsAccess::DENY,
    }
}

/// Non-secret operational env keys that pass through in the `sandbox: true`
/// curated baseline: PATH + system/locale/toolchain-discovery vars + the
/// build-hint `npm_config_*` subset. Ambient secrets never ride this list. The
/// exact baseline is the deferred build-jail thread's product surface; this is a
/// usable, safe default for the frontend-less engine.
///
/// The Windows container-essential block (`SystemRoot` … `PROCESSOR_ARCHITECTURE`)
/// is load-bearing: `CreateProcessW` with a constructed environment block that
/// omits `SystemRoot` fails `ERROR_ENVVAR_NOT_FOUND` (the loader resolves system
/// DLLs relative to it), and a normal Windows exe (node.exe) needs the
/// `USERPROFILE`/`APPDATA`/`LOCALAPPDATA` family to resolve its home/temp/config.
/// These names never appear on unix (the filter is over the ambient env, so the
/// baseline stays OS-appropriate without a `cfg`).
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
    // Windows container-essential (see the doc note above).
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];
const BASELINE_ENV_PREFIXES: &[&str] = &["LC_", "npm_config_"];

/// The `npm_config_` prefix carries BOTH build hints (kept) and registry
/// CREDENTIALS (must never reach sandboxed code). See [`is_npm_config_credential`].
const NPM_CONFIG_PREFIX: &str = "npm_config_";

/// Unambiguous credential words in an `npm_config_*` key — long/specific enough that
/// a case-insensitive SUBSTRING hit has no realistic collision with a node-gyp /
/// node-pre-gyp build hint (none of which embed these). Mirrors the env-name
/// [`SECRET_SUBSTR_TOKENS`] discipline, scoped to the registry-credential family.
const NPM_CRED_SUBSTR_TOKENS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "apikey",
];

/// Short/ambiguous credential words matched ONLY as a whole `_`/`-`/`.` segment, so
/// `npm_config_key` (npm's inline registry client SSL key) and `npm_config_api_key`
/// scrub while a package binary-host hint whose name merely contains the letters
/// (`npm_config_keytar_binary_host_mirror`, `..._monkey_...`) is spared. `auth` is
/// deliberately NOT here — it stays the anchored `_auth` test below so `always-auth` /
/// `_author` are not swept.
const NPM_CRED_SEGMENT_TOKENS: &[&str] = &["key"];

/// Whether an `npm_config_*` key (given the part AFTER the `npm_config_` prefix) is a
/// registry CREDENTIAL rather than a build hint — such keys must never reach sandboxed
/// lifecycle code (.fray/sandbox.md thread #6: registry auth never rides the lifecycle
/// env). Three tiers, all case-insensitive + delimiter-aware. First, the anchored legacy
/// markers: `_auth*` (the leading `_` spares `always-auth` / `_author`) and `email` as the
/// whole key or a registry-scoped `:email` suffix (an unanchored `email` would wrongly
/// scrub `npm_config_nodemailer_binary_host_mirror`). Then the unambiguous credential
/// words ([`NPM_CRED_SUBSTR_TOKENS`]) anywhere in the key — catching `password` /
/// `_authToken` / scoped `//host/:_password` and the undelimited `foo_token` / `my_secret`
/// forms an exact-segment rule would miss. Finally the short `key` family as a whole
/// segment ([`NPM_CRED_SEGMENT_TOKENS`]). Kept build hints
/// (`target`/`arch`/`runtime`/`nodedir`/`python`/`*_binary_host_mirror`/…) match none.
/// Best-effort per §8: the rare native package literally named after a credential word
/// loses its binary-host MIRROR hint (falling back to the default host), acceptable next
/// to leaking a token.
fn is_npm_config_credential(remainder: &str) -> bool {
    let r = remainder.to_ascii_lowercase();
    if r.contains("_auth") || r == "email" || r.ends_with(":email") {
        return true;
    }
    if NPM_CRED_SUBSTR_TOKENS
        .iter()
        .any(|w| word_in_substr(w, remainder))
    {
        return true;
    }
    NPM_CRED_SEGMENT_TOKENS
        .iter()
        .any(|w| word_is_segment(w, remainder))
}

/// Build the curated-baseline child env from the ambient env (the `sandbox: true`
/// / build-jail env posture). Only the non-secret operational allowlist passes.
pub fn curated_baseline_env(
    ambient: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    ambient
        .iter()
        .filter(|(k, _)| baseline_allows(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// OS-STARTUP-mechanism env names a sandboxed child needs merely to EXIST — the
/// spawning OS's own bootstrap essentials, per OS. Distinct from (and far narrower
/// than) [`BASELINE_ENV_EXACT`]: the baseline is "what a build child needs to
/// operate usefully" (PATH/HOME/USERPROFILE/npm hints); THIS is "what any child
/// needs before it can run at all."
///
/// Empirically pinned on the Windows VM (see the env-essentials thread) against
/// BOTH backend spawn paths — subsets tried until the true minimum that STARTS was
/// found — because the two paths need different things:
///  - `SystemRoot` — the loader/CLR essential. A real managed exe (`powershell.exe`)
///    launched with an EMPTY env block fails `rc=-65536` ("Internal Windows
///    PowerShell error / Loading managed…" — the CLR resolves `mscoree`/System32
///    relative to `SystemRoot`); `SystemRoot` ALONE flips it to rc=0. A native exe
///    (`node.exe`) happens to tolerate an empty block — but the sandbox must not
///    depend on every child being that lenient (the "starts only incidentally" gap).
///  - `LOCALAPPDATA` — the AppContainer essential. The ENFORCING path (fs/net
///    confined → a LowBox AppContainer) resolves the per-container profile dir
///    (`%LOCALAPPDATA%\Packages\…`) from the environment, so a block missing it fails
///    `CreateProcessW` with `ERROR_ENVVAR_NOT_FOUND` (203) BEFORE the child runs.
///    The VM subset sweep pinned this exactly: `{SystemRoot}` and
///    `{SystemRoot,USERPROFILE}` both still fail 203; `{SystemRoot,LOCALAPPDATA}` is
///    the smallest set that starts. `SystemDrive`/`windir`/`USERPROFILE`/`APPDATA`
///    are NOT needed to start and are deliberately NOT injected (no over-injection).
///
/// The set is injected on every strip-all (the compiler folds env before the backend
/// picks its path; `LOCALAPPDATA` is inert on the plain path, `SystemRoot` inert to
/// the AppContainer resolution — each is load-bearing on exactly one path). Both are
/// OS MECHANISM — an install-location and a profile-storage path pointer, never a
/// credential — so injecting them does not breach the deny-all floor (which denies
/// USER/ambient env + secrets, not the OS-startup essentials the process needs to
/// exist). `LOCALAPPDATA` embeds the OS username, but that disclosure is REDUNDANT
/// (the child runs AS that user and can already read its own username via its
/// SID/token/`whoami`, so the path leaks nothing new) and is empirically REQUIRED to
/// start an AppContainer child — injecting the real value is the minimal correct
/// choice. (A synthetic non-disclosing `LOCALAPPDATA` was considered and rejected: it
/// needs a real writable scratch dir + an fs grant, adding coupling for zero real
/// privacy gain given the redundancy.) POSIX has NO startup essential: an
/// absolute-path `execve` starts with an empty environ (`/usr/bin/true`, `sh`,
/// `node` all verified rc=0), so its list is empty and the floor injects nothing.
///
/// `#[cfg]`-gated on the SPAWNING OS (= the child's OS) so a POSIX floor provably
/// injects nothing regardless of ambient contents, while the selection logic stays
/// host-independently testable via [`os_essential_env_from`].
#[cfg(windows)]
const OS_ESSENTIAL_ENV: &[&str] = &["SystemRoot", "LOCALAPPDATA"];
#[cfg(not(windows))]
const OS_ESSENTIAL_ENV: &[&str] = &[];

/// Select the OS-essential names present in `ambient`, matched case-insensitively
/// (Windows env names are case-insensitive by OS contract — `SYSTEMROOT` and
/// `SystemRoot` are the same var — and the child keeps the ambient's actual cased
/// key + real value). Split from [`os_essential_env`] so the selection is unit-
/// testable on any host by passing an explicit name list.
fn os_essential_env_from(
    ambient: &std::collections::BTreeMap<String, String>,
    names: &[&str],
) -> std::collections::BTreeMap<String, String> {
    ambient
        .iter()
        .filter(|(k, _)| names.iter().any(|n| n.eq_ignore_ascii_case(k)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The OS-essential env for the spawning OS, read from the host ambient env at
/// compile time. Only the whitelisted NAMES are admitted; their VALUES come from
/// the real ambient env, and an essential absent from the host is skipped (never
/// fabricated).
pub fn os_essential_env(
    ambient: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    os_essential_env_from(ambient, OS_ESSENTIAL_ENV)
}

/// The strip-all env FLOOR: an enforcing env that WITHHOLDS all user/ambient env
/// but injects the minimal OS-startup essentials so the child spawns reliably
/// instead of only where the OS tolerates an empty block. Injecting these does NOT
/// breach the deny-all floor — the floor denies USER/ambient env and secrets; the
/// essentials are OS MECHANISM (where Windows is installed / how its loader finds
/// System32), never user config or a credential. Single source of truth for both
/// strip-all constructors: the complete-statement floor (`floor_env`) and the
/// explicit `env: false`.
pub fn strip_all_env(
    ambient: &std::collections::BTreeMap<String, String>,
) -> crate::policy::EnvPolicy {
    let constructed = os_essential_env(ambient);
    let withheld = ambient
        .keys()
        .filter(|k| !constructed.contains_key(*k))
        .cloned()
        .collect();
    crate::policy::EnvPolicy {
        enforce: true,
        constructed,
        schema: Vec::new(),
        withheld,
    }
}

/// Case-insensitive prefix strip: returns the remainder after `prefix` if `key`
/// starts with it (ignoring ASCII case), else `None`. Used to gate the credential
/// carve-out uniformly across platforms.
fn strip_prefix_ci<'a>(key: &'a str, prefix: &str) -> Option<&'a str> {
    key.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &key[prefix.len()..])
}

/// Whether a key is in the curated baseline. Case-SENSITIVE on unix (POSIX env
/// keys are); case-INSENSITIVE on Windows, where env names are case-insensitive by
/// OS contract and a process may report `SYSTEMROOT` or `SystemRoot` — an
/// exact-case miss would drop a container-essential var and re-open the
/// `ERROR_ENVVAR_NOT_FOUND` spawn failure the baseline exists to prevent.
///
/// Public because the env `"..."` fold reuses it as the match predicate for the
/// curated-baseline allow entry — so `env: ["..."]` and `sandbox: true`'s env are
/// the SAME allowlist by construction (single source of truth), never a drifting
/// reimplementation.
pub fn baseline_allows(key: &str) -> bool {
    // Registry credential keys ride the build-hint `npm_config_*` prefix; scrub them
    // before the prefix pass would admit them. Case-insensitive prefix match so a
    // Windows-cased `NPM_CONFIG_//…:_authToken` is caught too (env names are
    // case-insensitive there); on unix npm always emits the lowercase prefix, so a CI
    // match only ever affects `npm_config_`-shaped keys and never widens the allow.
    if let Some(rest) = strip_prefix_ci(key, NPM_CONFIG_PREFIX)
        && is_npm_config_credential(rest)
    {
        return false;
    }
    #[cfg(windows)]
    {
        BASELINE_ENV_EXACT
            .iter()
            .any(|e| e.eq_ignore_ascii_case(key))
            || BASELINE_ENV_PREFIXES.iter().any(|p| {
                key.get(..p.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(p))
            })
    }
    #[cfg(not(windows))]
    {
        BASELINE_ENV_EXACT.contains(&key)
            || BASELINE_ENV_PREFIXES.iter().any(|p| key.starts_with(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn homes() -> Homes {
        Homes {
            home: PathBuf::from("/testhome"),
            tmp: PathBuf::from("/testtmp"),
            cache: PathBuf::from("/testhome/.cache"),
            project: PathBuf::from("/proj"),
        }
    }

    #[test]
    fn baseline_keeps_windows_essentials_drops_secrets() {
        let ambient: BTreeMap<String, String> = [
            ("PATH", "/bin"),
            ("USERPROFILE", "C:/Users/me"),
            ("LOCALAPPDATA", "C:/Users/me/AppData/Local"),
            ("APPDATA", "C:/Users/me/AppData/Roaming"),
            ("NUMBER_OF_PROCESSORS", "8"),
            ("PROCESSOR_ARCHITECTURE", "AMD64"),
            ("SystemRoot", "C:/Windows"),
            ("MY_SECRET_TOKEN", "leak"),
            ("AWS_SECRET_ACCESS_KEY", "leak"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let out = curated_baseline_env(&ambient);
        for k in [
            "PATH",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
            "SystemRoot",
        ] {
            assert!(out.contains_key(k), "baseline must keep {k}");
        }
        assert!(
            !out.contains_key("MY_SECRET_TOKEN"),
            "secret not in baseline"
        );
        assert!(
            !out.contains_key("AWS_SECRET_ACCESS_KEY"),
            "aws secret not in baseline"
        );
    }

    fn ambient(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn os_essential_selection_is_case_insensitive_and_value_preserving() {
        // Host-independent: exercise the selection with an explicit name list so the
        // Windows contract (case-insensitive names, real ambient value kept) is proven
        // on any dev host. A non-essential name is never admitted.
        let env = ambient(&[
            ("SYSTEMROOT", "C:/Windows"), // upper-cased ambient key still matches
            ("windir", "C:/Windows"),
            ("SECRET_TOKEN", "leak"),
        ]);
        let out = os_essential_env_from(&env, &["SystemRoot", "windir"]);
        assert_eq!(
            out.get("SYSTEMROOT").map(String::as_str),
            Some("C:/Windows"),
            "case-insensitive name match keeps the ambient key + value"
        );
        assert!(out.contains_key("windir"));
        assert!(
            !out.contains_key("SECRET_TOKEN"),
            "a non-essential (secret) name is never injected"
        );
    }

    #[test]
    fn strip_all_injects_only_essentials_and_withholds_the_rest() {
        // The security property that matters: an ambient secret must NEVER ride the
        // strip-all floor's constructed env; only whitelisted OS essentials do, and
        // everything else is recorded withheld. On POSIX the essential set is empty,
        // so `constructed` is empty and even a `SystemRoot`-named ambient var is
        // withheld — the floor injects nothing where the OS needs nothing.
        let env = ambient(&[
            ("SystemRoot", "C:/Windows"),
            ("LOCALAPPDATA", "C:/Users/me/AppData/Local"),
            ("AWS_SECRET_ACCESS_KEY", "leak"),
            ("GITHUB_TOKEN", "leak"),
            ("PATH", "/bin"),
        ]);
        let p = strip_all_env(&env);
        assert!(p.enforce, "strip-all always enforces");
        // No secret or user-config var ever appears in the constructed child env.
        for secret in ["AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "PATH"] {
            assert!(
                !p.constructed.contains_key(secret),
                "{secret} must never ride the strip-all floor"
            );
            assert!(
                p.withheld.contains(&secret.to_string()),
                "{secret} must be recorded withheld"
            );
        }
        #[cfg(windows)]
        {
            for essential in ["SystemRoot", "LOCALAPPDATA"] {
                assert!(
                    p.constructed.contains_key(essential),
                    "Windows floor injects the OS-startup essential {essential}"
                );
                assert!(
                    !p.withheld.contains(&essential.to_string()),
                    "an injected essential is provided, not withheld"
                );
            }
        }
        #[cfg(not(windows))]
        {
            assert!(
                p.constructed.is_empty(),
                "POSIX floor injects no essentials (empty-env exec starts fine)"
            );
            assert!(
                p.withheld.contains(&"SystemRoot".to_string()),
                "on POSIX a SystemRoot-named ambient var is withheld, not injected"
            );
        }
    }

    #[test]
    fn npm_config_build_hints_pass_but_credentials_scrubbed() {
        // The `npm_config_*` family passes build hints through, but registry auth
        // rides the same prefix and must be scrubbed — thread #6. Both the bare
        // legacy keys and the registry-scoped `//host/:_auth…` forms are excluded.
        // Build hints (kept) — incl. two regression guards for false positives: a
        // package whose name embeds "email" (`nodemailer`) must survive the anchored
        // `email` marker, and a package whose name embeds "key" (`keytar`) must survive
        // the whole-SEGMENT `key` rule. `always-auth` is `-auth`, not `_auth` — kept.
        let hints = [
            "npm_config_target",
            "npm_config_arch",
            "npm_config_target_arch",
            "npm_config_runtime",
            "npm_config_nodedir",
            "npm_config_python",
            "npm_config_build_from_source",
            "npm_config_registry",
            "npm_config_sharp_binary_host",
            "npm_config_nodemailer_binary_host_mirror",
            "npm_config_keytar_binary_host_mirror",
            "npm_config_always-auth",
        ];
        // Credentials (scrubbed) — the anchored legacy markers, the broadened
        // credential-word set (token/secret/password/passwd/credential/apikey), and
        // the short `key` family as a delimited segment. Covers undelimited, hyphen,
        // and dot forms an exact-segment rule would miss.
        let creds = [
            "npm_config__auth",
            "npm_config__authToken",
            "npm_config__password",
            "npm_config_email",
            "npm_config_//registry.npmjs.org/:_authToken",
            "npm_config_//registry.npmjs.org/:_password",
            "npm_config_//registry.npmjs.org/:_auth",
            "npm_config_password",
            "npm_config_passwd",
            "npm_config_foo_token",
            "npm_config_authtoken",
            "npm_config_my_secret",
            "npm_config_credential",
            "npm_config_apikey",
            "npm_config_api_key",
            "npm_config_signing_key",
            "npm_config_key",
            "npm_config_my-token",
            "npm_config_x.secret.y",
        ];
        let ambient: BTreeMap<String, String> = hints
            .iter()
            .chain(creds.iter())
            .map(|k| (k.to_string(), "v".to_string()))
            .collect();
        let out = curated_baseline_env(&ambient);
        for k in hints {
            assert!(out.contains_key(k), "build hint {k} must pass");
        }
        for k in creds {
            assert!(!out.contains_key(k), "credential {k} must be scrubbed");
        }
    }

    #[test]
    fn secret_denies_include_the_new_additions() {
        let globs: Vec<String> = secret_read_denies(&homes())
            .into_iter()
            .map(|r| r.matcher.as_str().to_string())
            .collect();
        // Depth-independent `.env`/`.envrc` globs are verbatim (never anchored).
        for g in ["**/.env", "**/.env/**", "**/.env.*/**", "**/.envrc"] {
            assert!(globs.contains(&g.to_string()), "missing verbatim deny {g}");
        }
        // Home-anchored secret files/dirs appear as subtree denies (substring match
        // tolerates OS firmlink canonicalization of the fake home prefix).
        for frag in [".gnupg", ".pgpass", ".pypirc", ".config/git/credentials"] {
            assert!(
                globs.iter().any(|g| g.contains(frag)),
                "missing home secret deny containing {frag}"
            );
        }
    }
}
