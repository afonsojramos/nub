//! `nub-sandbox` — nub's OS-enforced sandbox engine.
//!
//! The build-jail PROFILE is the first consumer: a default-ON jail for
//! dependency lifecycle scripts (install/postinstall/…) that blocks the
//! supply-chain attack canon (secret exfil, C2/beacon egress, out-of-package
//! writes, persistence) while letting legitimate native builds (node-gyp /
//! prebuild-install) through. See `.fray/build-jail-design.md` (the design) and
//! `.fray/sandbox.md` (the engine architecture: this crate is step 1 — the
//! build-jail profile at parity with today's aube jail, before the runtime
//! profile).
//!
//! ## Shape
//! - [`SandboxPolicy`] — the unified `{env, fs, net, pid}` model (one schema,
//!   two future profiles).
//! - [`build_jail`] — the build-jail preset that assembles a policy from the
//!   §3/§4/§5/§8.5 defaults.
//! - [`apply_to_command`] — apply a policy to a `std::process::Command`: the
//!   env-axis scrub (a spawn-boundary filter) + the per-OS fs/net/pid backend.
//!
//! ## Enforcement posture (security-critical)
//! - **Deny-by-default on env / fs-write / net** once the build-jail policy is
//!   active (it always is, default-ON). The env allowlist clears the child env
//!   and re-admits ONLY known-safe keys; fs-write is confined; net egress is
//!   confined to the allowlist.
//! - **Fail-SAFE, not fail-open.** A missing OS primitive degrades the affected
//!   axis and surfaces a one-line WARNING (never silent, never a hard install
//!   failure) — but a backend NEVER silently claims enforcement it didn't
//!   deliver. The env-scrub (not an OS primitive) always applies on every OS.

pub mod backend;
pub mod build_jail;
pub mod net_defaults;
pub mod policy;
pub mod secrets;

pub use backend::Degradation;
pub use build_jail::{BuildJailParams, default_extra_write};
pub use policy::{EnvPolicy, FsPolicy, NetPolicy, PidPolicy, SandboxPolicy};

use std::process::Command;

/// Apply the env-axis scrub to `cmd`: clear the child env and re-admit ONLY the
/// keys the policy admits, from `inherited` (the env to filter — typically the
/// parent process's `std::env::vars`). The caller is responsible for re-adding
/// nub's own required plumbing (NODE / NODE_OPTIONS / proxy URLs / the manifest
/// `npm_package_*`) AFTER this call — those are injected, not inherited, so they
/// are not subject to the allowlist.
///
/// This is the spawn-boundary filter, not an OS primitive — it runs on every OS
/// identically, so the env guarantee holds even where the fs/net backend
/// degrades. When `policy.env.enforce` is false, this is a no-op (the child
/// inherits the full env, today's unjailed behavior).
pub fn apply_env_scrub<I, K, V>(cmd: &mut Command, policy: &EnvPolicy, inherited: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str> + Into<std::ffi::OsString>,
    V: Into<std::ffi::OsString>,
{
    if !policy.enforce {
        return;
    }
    cmd.env_clear();
    for (k, v) in inherited {
        if policy.admits(k.as_ref()) {
            cmd.env(k.into(), v.into());
        }
    }
}

/// Apply the full policy to `cmd`: the OS fs/net/pid backend (env is applied
/// separately via [`apply_env_scrub`] because it needs the caller's inherited
/// env + injected plumbing). Returns the [`Degradation`] so the caller can
/// surface a reduced-mode WARNING.
///
/// On macOS this REWRITES `cmd` to run under `sandbox-exec` (carrying the env +
/// cwd already set on `cmd`), so call this AFTER [`apply_env_scrub`] and after
/// injecting nub's plumbing env. On Linux it installs a `pre_exec` Landlock +
/// seccomp hook. On Windows it is currently a reporting no-op (TODO: restricted
/// token — see [`backend::stub`]).
pub fn apply_to_command(cmd: &mut Command, policy: &SandboxPolicy) -> std::io::Result<Degradation> {
    backend::apply(cmd, policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_scrub_clears_and_readmits_allowlist_only() {
        let mut cmd = Command::new("true");
        let pol = EnvPolicy {
            allow_exact: vec!["PATH".into(), "HOME".into()],
            allow_prefix: vec!["npm_config_".into()],
            deny_substring: vec!["token".into()],
            enforce: true,
        };
        let inherited: Vec<(String, String)> = vec![
            ("PATH".into(), "/usr/bin".into()),
            ("HOME".into(), "/home/me".into()),
            ("npm_config_registry".into(), "https://r".into()),
            ("AWS_SECRET_ACCESS_KEY".into(), "leak".into()),
            ("npm_config_authToken".into(), "leak".into()),
            ("RANDOM".into(), "x".into()),
        ];
        apply_env_scrub(&mut cmd, &pol, inherited);
        let envs: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
            .collect();
        assert!(envs.contains_key(std::ffi::OsStr::new("PATH")));
        assert!(envs.contains_key(std::ffi::OsStr::new("npm_config_registry")));
        // the secret-bearing and unknown keys are gone
        assert!(!envs.contains_key(std::ffi::OsStr::new("AWS_SECRET_ACCESS_KEY")));
        assert!(!envs.contains_key(std::ffi::OsStr::new("npm_config_authToken")));
        assert!(!envs.contains_key(std::ffi::OsStr::new("RANDOM")));
    }

    #[test]
    fn env_scrub_noop_when_not_enforced() {
        let mut cmd = Command::new("true");
        let pol = EnvPolicy::default(); // enforce=false
        apply_env_scrub(
            &mut cmd,
            &pol,
            vec![("RANDOM".to_string(), "x".to_string())],
        );
        // no env_clear happened; the inherited var is NOT explicitly set
        // (it would inherit naturally at spawn). We just assert no panic + the
        // command is unmodified in the no-enforce path.
        let count = cmd.get_envs().count();
        assert_eq!(count, 0);
    }
}
