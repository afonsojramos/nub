//! Per-OS enforcement backends and the [`apply`] entry that turns a resolved
//! [`SandboxPolicy`] into a launch-ready child.
//!
//! The enforcement contract is FAIL-SAFE-WITH-DEGRADATION, not fail-open (ported
//! from the reviewed salvage `backend/mod.rs`): a backend NEVER silently drops an
//! axis it claimed to enforce. When a primitive is unavailable it records the
//! loss in [`Degradation`] so the caller surfaces a WARNING; a hard fail-closed
//! (a required axis unenforceable) is `Err(Degradation)`.
//!
//! BACKEND STATUS: macOS (Seatbelt, [`macos`]), Linux (Landlock+seccomp,
//! [`linux`]), and Windows (AppContainer LowBox, [`windows`]) are wired; any other
//! OS runs the env-scrub-only [`generic_apply`] skeleton — which constructs the
//! child env and reports fs/net as NOT enforced. Every path preserves the API shape
//! (`apply(policy, spec) -> Result<Prepared, Degradation>`) the future embedder
//! seam slots into.
//!
//! LAUNCH SEAM: a backend either configures [`Prepared::command`] for the caller to
//! spawn (macOS wraps `sandbox-exec`; Linux installs a `pre_exec` hook; the skeleton
//! just scrubs env) OR — Windows only — OWNS the whole spawn lifecycle, because an
//! AppContainer launch cannot be a pre-built `std::process::Command`: it needs a
//! custom `CreateProcessW` with `STARTUPINFOEX`/`SECURITY_CAPABILITIES`, a Job Object
//! assigned at creation, and per-run ACL grants that must be TORN DOWN after the
//! child exits. [`Prepared::status`] is the uniform verb: mac/linux/skeleton delegate
//! to `command.status()`; Windows runs its launcher (setup → spawn → wait → RAII
//! teardown) when a launch plan is attached.

use crate::policy::SandboxPolicy;
use std::process::Command;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

// The Windows AppContainer backend. Compiled on Windows (its real consumer) and
// under `test` on any host — so its OS-agnostic IR→plan derivation (grant carve,
// capability selection, dangerous-root guard) is unit-tested on the macOS dev host
// without a Windows machine (the FFI launcher itself stays `#[cfg(windows)]`).
#[cfg(any(target_os = "windows", test))]
mod windows;

// The OS-agnostic Landlock grant derivation. Compiled on Linux (its real consumer)
// and under `test` on any host — so its security-critical carve logic is unit-tested
// on the macOS dev host over tempfile trees, without a kernel.
#[cfg(any(target_os = "linux", test))]
mod linux_grants;

/// Which confinement axes a backend managed to enforce, and which degraded. A
/// non-empty `lost` becomes a user-facing WARNING. Ported contract.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Degradation {
    /// Axis names that could NOT be enforced (e.g. "fs", "net", "net-per-host").
    pub lost: Vec<String>,
    /// A one-line reason (missing primitive, unsupported OS), surfaced with the
    /// lost-axis list.
    pub reason: Option<String>,
}

impl Degradation {
    /// Full enforcement — nothing lost.
    pub fn full() -> Self {
        Self::default()
    }
    pub fn is_full(&self) -> bool {
        self.lost.is_empty()
    }
    /// The one-line WARNING text, or `None` when fully enforced.
    pub fn warning(&self) -> Option<String> {
        if self.lost.is_empty() {
            return None;
        }
        let axes = self.lost.join(", ");
        Some(match &self.reason {
            Some(r) => format!("sandbox running in reduced mode — {axes} not enforced ({r})"),
            None => format!("sandbox running in reduced mode — {axes} not enforced"),
        })
    }
}

/// The command to launch under a policy. Host-provided (Boundary B).
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: std::ffi::OsString,
    pub args: Vec<std::ffi::OsString>,
    /// Working directory for the child, if the caller pins one.
    pub cwd: Option<std::path::PathBuf>,
}

impl CommandSpec {
    pub fn new(program: impl Into<std::ffi::OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
        }
    }
    pub fn arg(mut self, a: impl Into<std::ffi::OsString>) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    pub fn cwd(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// A launch-ready child: a configured [`Command`] plus the [`Degradation`] the
/// backend achieved. The caller surfaces `degradation`, then launches with
/// [`Prepared::status`] (NOT `command.status()` directly — Windows enforcement
/// rides the `status` seam, not the `command` field).
pub struct Prepared {
    /// The configured child for the mac/linux/skeleton path. On Windows this is the
    /// env-scrubbed plain child used ONLY when nothing needs AppContainer confinement
    /// (`launch` is `None`); when confinement applies, `launch` owns the spawn and
    /// this field is unused.
    pub command: Command,
    pub degradation: Degradation,
    /// Windows AppContainer launch plan — the backend owns spawn+wait+teardown when
    /// this is `Some`. Absent (or on other OSes) → [`Prepared::status`] spawns
    /// `command`.
    #[cfg(target_os = "windows")]
    pub(crate) launch: Option<windows::WindowsLaunch>,
}

impl Prepared {
    /// Launch the prepared child and wait for it, returning its exit status. The
    /// UNIFORM launch verb across backends: mac/linux/skeleton spawn `command`;
    /// Windows runs its AppContainer launcher (ACL setup → `CreateProcessW` under a
    /// LowBox token → wait → RAII teardown) when a launch plan is attached.
    pub fn status(mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(target_os = "windows")]
        if let Some(launch) = self.launch.take() {
            return launch.run();
        }
        self.command.status()
    }
}

/// Apply a resolved policy to a command, dispatching to the per-OS backend.
///
/// The env axis is enforced by CONSTRUCTION (not an OS primitive): when the policy
/// enforces env, the child env is cleared and set to exactly the policy's
/// constructed map. Linux additionally hardens a scrubbed env with two OS primitives
/// so the withheld secret can't be recovered from a co-resident ancestor — seccomp
/// denies `ptrace`/`process_vm_readv` (memory scrape) and Landlock closes `/proc`
/// (the `/proc/<ppid>/environ` file); macOS cannot block the analogous
/// `KERN_PROCARGS2` sysctl, so its env-scrub is best-effort vs co-resident code (a
/// launcher-level concern). fs/net enforcement is the backend's job; on an OS whose
/// backend has not landed, [`generic_apply`] reports them as not-enforced (never
/// silent).
pub fn apply(policy: &SandboxPolicy, spec: CommandSpec) -> Result<Prepared, Degradation> {
    #[cfg(target_os = "macos")]
    {
        macos::apply(policy, spec)
    }
    #[cfg(target_os = "linux")]
    {
        linux::apply(policy, spec)
    }
    #[cfg(target_os = "windows")]
    {
        windows::apply(policy, spec)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        generic_apply(policy, spec)
    }
}

/// Env-scrub-only skeleton for an OS with no wired backend. Reports fs and net as
/// not-enforced so a caller never mistakes the skeleton for confinement.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn generic_apply(policy: &SandboxPolicy, spec: CommandSpec) -> Result<Prepared, Degradation> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }

    // Env axis — construction, not interception.
    if policy.env.enforce {
        command.env_clear();
        for (k, v) in &policy.env.constructed {
            command.env(k, v);
        }
    }

    // fs/net: honestly report what the skeleton does not yet enforce.
    let mut lost = Vec::new();
    if fs_confines(policy) {
        lost.push("fs".to_string());
    }
    if policy.net.enforce {
        lost.push("net".to_string());
    }
    let degradation = if lost.is_empty() {
        Degradation::full()
    } else {
        Degradation {
            lost,
            reason: Some("no OS backend wired in this build (Stage 1)".to_string()),
        }
    };
    Ok(Prepared {
        command,
        degradation,
    })
}

/// Whether the fs policy actually confines anything (a non-relaxed base or any
/// entry). A relaxed fs axis (allow-all, no rules) is not a lost enforcement.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn fs_confines(policy: &SandboxPolicy) -> bool {
    !matches!(policy.fs.rules.default_effect, crate::policy::Effect::Allow)
        || !policy.fs.rules.entries.is_empty()
}
