//! Optional embedder hook for an extract-time package scanner.
//!
//! A process-global set-once cell, mirroring `aube_settings::set_embedder_defaults`:
//! standalone aube never registers a hook, so [`run_extract_hook`] is a no-op and
//! the default path pulls in no scanner dependency and stays byte-identical. The
//! concrete scanner (an oxc-based parser) lives entirely in the embedder; aube
//! only invokes an opaque closure, so no parser weight leaks into the store crate.

use crate::PackageIndex;
use std::sync::OnceLock;

/// A scanner the embedder runs against a freshly-imported package's file index
/// at CAS-extract time. Invoked at the end of `import_tarball_reader`, on the
/// fetch/blocking fan-out thread, so per-version analysis overlaps ongoing
/// downloads. `Send + Sync` because tarball import fans out across rayon workers.
type ExtractHook = Box<dyn Fn(&PackageIndex) + Send + Sync>;

static EXTRACT_HOOK: OnceLock<ExtractHook> = OnceLock::new();

/// Register the extract-time scan hook. Set-once; later calls are ignored.
pub fn set_extract_hook(hook: ExtractHook) {
    let _ = EXTRACT_HOOK.set(hook);
}

/// Invoke the registered extract hook, if any. No-op for standalone aube (one
/// relaxed atomic load, then return).
pub(crate) fn run_extract_hook(index: &PackageIndex) {
    if let Some(hook) = EXTRACT_HOOK.get() {
        hook(index);
    }
}
