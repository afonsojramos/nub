//! Per-OS enforcement backends. Each takes a [`SandboxPolicy`](crate::SandboxPolicy)
//! and applies it to a child process at spawn time.
//!
//! The enforcement contract is FAIL-SAFE-with-degradation, not fail-open:
//! - When a backend's primitives are available, the policy is enforced and the
//!   spawn proceeds.
//! - When a primitive is UNAVAILABLE (old kernel, no Landlock, etc.), the
//!   backend returns a [`Degradation`] describing exactly which axes were lost
//!   so the caller can surface a one-line WARNING (never silent). The install
//!   still runs — a PM install cannot hard-fail on a missing hardening layer
//!   (`.fray/build-jail-design.md` §7), but the loss is always reported.
//! - A backend NEVER silently drops an axis it claimed to enforce.
//!
//! Today the macOS (Seatbelt) backend is the fully-implemented reference (the
//! primary dev OS). Linux (Landlock + seccomp) is implemented; Windows is
//! scaffolded with an explicit TODO (Tier 0 Job-Object + env-scrub only until
//! the restricted-token write-jail lands).

use crate::policy::SandboxPolicy;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub mod stub;

/// Which confinement axes a backend managed to enforce on this host, and which
/// degraded. The caller turns a non-empty `lost` into a user-facing WARNING.
#[derive(Debug, Clone, Default)]
pub struct Degradation {
    /// Human-readable axis names that could NOT be enforced (e.g. "fs",
    /// "net-per-host"). Empty = full enforcement.
    pub lost: Vec<String>,
    /// A one-line reason for the degradation (kernel version, missing
    /// primitive), surfaced alongside the lost-axis list.
    pub reason: Option<String>,
}

impl Degradation {
    pub fn full() -> Self {
        Self::default()
    }
    pub fn is_full(&self) -> bool {
        self.lost.is_empty()
    }
    /// Render the one-line WARNING text, or `None` when fully enforced.
    pub fn warning(&self) -> Option<String> {
        if self.lost.is_empty() {
            return None;
        }
        let axes = self.lost.join(", ");
        match &self.reason {
            Some(r) => Some(format!(
                "build sandbox running in reduced mode — {axes} not enforced ({r})"
            )),
            None => Some(format!(
                "build sandbox running in reduced mode — {axes} not enforced"
            )),
        }
    }
}

/// The OS-agnostic backend entry. Applies `policy` to the in-construction
/// child command (Unix: installs a `pre_exec` hook / on macOS wraps the argv);
/// returns the [`Degradation`] so the caller can warn.
///
/// NOTE: the env-axis scrub is applied by the CALLER on the command's env (it
/// is not an OS primitive — it is a spawn-boundary filter), using
/// [`crate::apply_env_scrub`]. The backend handles the fs/net/pid OS layers.
#[cfg(unix)]
pub fn apply(
    cmd: &mut std::process::Command,
    policy: &SandboxPolicy,
) -> std::io::Result<Degradation> {
    #[cfg(target_os = "macos")]
    {
        macos::apply(cmd, policy)
    }
    #[cfg(target_os = "linux")]
    {
        linux::apply(cmd, policy)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (cmd, policy);
        Ok(Degradation {
            lost: vec!["fs".into(), "net".into()],
            reason: Some("no unprivileged sandbox backend for this OS".into()),
        })
    }
}

/// Windows entry — Tier 0 (env-scrub + Job-Object limits) is the caller's job
/// via the existing reaping path; the OS write-jail (restricted token) is a
/// scaffolded TODO. Returns a degradation describing the gap honestly.
#[cfg(not(unix))]
pub fn apply(
    cmd: &mut std::process::Command,
    policy: &SandboxPolicy,
) -> std::io::Result<Degradation> {
    stub::apply(cmd, policy)
}
