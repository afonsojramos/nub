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
//! STAGE 1 (this build): compiler + IR + matcher are complete and exhaustively
//! tested; [`apply`] is the env-scrub-only skeleton (OS backends land S3–S5). The
//! [`conformance`] harness evaluates compiler/matcher verdicts against committed
//! fixtures — the engine-pure half of the cross-platform conformance bar.

pub mod backend;
pub mod compiler;
pub mod conformance;
pub mod matcher;
pub mod policy;

pub use backend::{CommandSpec, Degradation, Prepared, apply};
pub use compiler::{CommandRunner, CompileCtx, CompileError, compile};
pub use matcher::Homes;
pub use policy::SandboxPolicy;
