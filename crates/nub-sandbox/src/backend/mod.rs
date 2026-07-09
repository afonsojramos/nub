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

use crate::policy::{Effect, SandboxPolicy};
use crate::proxy::{EgressProxy, StaticDecider};
use std::process::Command;
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

// The unprivileged seccomp USER_NOTIF supervisor that closes the port-scoped
// ConnectTcp residual in NetMode::Proxy (Linux-only mechanism; see the module doc).
#[cfg(target_os = "linux")]
mod linux_connect_notify;

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
    /// The running egress proxy (design.md §2.5), when the policy enforces per-host
    /// net. It runs in the nub PARENT and MUST outlive the child, so it is owned here:
    /// [`Prepared::status`] holds it for the child's whole run, and dropping this
    /// value stops the listener. `None` when net is unconfined or coarse-deny (no
    /// proxy needed). Set by [`apply`], not the per-OS backends.
    pub(crate) proxy: Option<EgressProxy>,
    /// Linux per-host connect-notify supervisor (closes the ConnectTcp port residual).
    /// When `Some`, [`Prepared::status`] spawns `command`, receives the child's seccomp
    /// listener fd, and runs the supervisor for the child's lifetime instead of a plain
    /// `command.status()`. Set only by [`linux::apply`] in `NetMode::Proxy` (and only
    /// where the supervisor is viable); `None` otherwise.
    #[cfg(target_os = "linux")]
    pub(crate) connect_notify: Option<linux_connect_notify::ConnectNotify>,
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
    ///
    /// The egress proxy (`self.proxy`) is held for the child's whole run and dropped
    /// (listener shut down) only after the child exits — `self` owns it until this
    /// method returns.
    pub fn status(mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(target_os = "windows")]
        if let Some(launch) = self.launch.take() {
            return launch.run();
        }
        // Linux per-host: run the connect-notify supervisor over the child's lifetime.
        // `self` (holding `self.proxy`) outlives this call, so the egress proxy stays up.
        #[cfg(target_os = "linux")]
        if let Some(cn) = self.connect_notify.take() {
            return cn.run(&mut self.command);
        }
        self.command.status()
    }
}

/// Whether the policy needs the per-host egress proxy: net enforced AND at least one
/// Allow rule (a pure deny-all is coarse — no proxy, nothing is reachable). A proxy
/// that fails to start degrades to coarse-deny (fail-SAFE: denies more, not less), so
/// this returns `None` on a start failure and the backend reports `net-per-host`.
fn start_proxy_if_needed(policy: &SandboxPolicy) -> Option<EgressProxy> {
    if policy.net.enforce && policy.net.rules.iter().any(|r| r.effect == Effect::Allow) {
        let decider = Arc::new(StaticDecider::new(policy.net.clone()));
        EgressProxy::start(decider).ok()
    } else {
        None
    }
}

/// The cooperative proxy-env hint set on the child so ordinary HTTP(S) clients route
/// through the loopback proxy. NOT the boundary (a malicious client ignores it — the
/// OS deny-layer forces the traffic through); numeric host so the child needs no name
/// resolution. Both upper/lower case (tools split on which they read).
fn set_proxy_env(command: &mut Command, port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
    ] {
        command.env(key, &url);
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
    // Start the per-host egress proxy FIRST (if the policy needs it), so its bound port
    // is threaded into the backend deny-layer (which permits egress ONLY to the proxy
    // endpoint) before the child is prepared. The proxy is then stashed on `Prepared`
    // so it outlives the child (design.md §2.5).
    let proxy = start_proxy_if_needed(policy);
    let proxy_port = proxy.as_ref().map(EgressProxy::port);

    #[cfg(target_os = "macos")]
    let mut prepared = macos::apply(policy, spec, proxy_port)?;
    #[cfg(target_os = "linux")]
    let mut prepared = linux::apply(policy, spec, proxy_port)?;
    #[cfg(target_os = "windows")]
    let mut prepared = windows::apply(policy, spec, proxy_port)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let mut prepared = generic_apply(policy, spec, proxy_port)?;

    prepared.proxy = proxy;
    Ok(prepared)
}

/// Env-scrub-only skeleton for an OS with no wired backend. Reports fs and net as
/// not-enforced so a caller never mistakes the skeleton for confinement.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn generic_apply(
    policy: &SandboxPolicy,
    spec: CommandSpec,
    proxy_port: Option<u16>,
) -> Result<Prepared, Degradation> {
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
    if let Some(port) = proxy_port {
        set_proxy_env(&mut command, port);
    }

    // fs/net: honestly report what the skeleton does not yet enforce. The skeleton has
    // NO OS deny-layer, so even with the proxy running it cannot FORCE the child
    // through it — net is reported unenforced regardless.
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
        proxy: None,
    })
}

/// Whether the fs policy actually confines anything (a non-relaxed base or any
/// entry). A relaxed fs axis (allow-all, no rules) is not a lost enforcement.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn fs_confines(policy: &SandboxPolicy) -> bool {
    !matches!(policy.fs.rules.default_effect, crate::policy::Effect::Allow)
        || !policy.fs.rules.entries.is_empty()
}
