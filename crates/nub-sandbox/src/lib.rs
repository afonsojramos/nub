//! nub-sandbox — the OS-enforced sandbox ENGINE, PM-pure by construction.
//!
//! The public surface is two functions over already-parsed data (the two
//! boundaries of design.md §2):
//!   - [`compile`] — surface `sandbox` JSON + [`CompileCtx`] → resolved
//!     [`SandboxPolicy`] (Boundary A: all surface syntax discharged).
//!   - [`apply`] — [`SandboxPolicy`] + [`CommandSpec`] → a launch-ready
//!     [`Prepared`] child, or a fail-closed [`Degradation`] (Boundary B: no
//!     nub-cli/aube/PM type crosses this line — the future build-jail embedder
//!     seam wires aube's lifecycle to these two fns).
//!
//! BACKEND STATUS: the compiler + IR + matcher are complete and exhaustively
//! tested. [`apply`] enforces fs/net/env on macOS (Seatbelt), real-kernel Linux
//! (Landlock + seccomp), and Windows (AppContainer), each proven by per-axis
//! enforcement tests with negative controls; per-host egress rides the localhost
//! proxy. The [`conformance`] harness evaluates compiler/matcher verdicts against
//! committed fixtures — the engine-pure half of the cross-platform bar.
//!
//! What the engine does NOT close (bounded residuals + the launcher-handoff
//! contract) is recorded honestly in `LIMITATIONS.md` alongside the runtime
//! [`Degradation`] signals; read it before relying on any single-axis guarantee.

pub mod backend;
pub mod compiler;
pub mod conformance;
pub mod matcher;
pub mod policy;
pub mod proxy;

pub use backend::{CommandSpec, Degradation, Prepared, apply};
pub use compiler::{
    CommandRunner, CompileCtx, CompileError, CompileWarning, compile, compile_with_warnings,
};
pub use matcher::Homes;
pub use policy::SandboxPolicy;
pub use proxy::{Decision, EgressProxy, GrantDecider, Host, StaticDecider};
