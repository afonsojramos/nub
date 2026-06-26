//! Single-binary runtime extraction (the `embed-runtime` feature only).
//!
//! In single-binary mode the whole `runtime/` tree (preload scripts + vendored
//! `node_modules` + the platform `nub-native.node`) is embedded in the binary as
//! a zstd-19 tar blob (see `build.rs`). On first run we inflate it ONCE to a
//! versioned cache dir and hand `find_preload` that path; every later run finds
//! the dir already present and pays a single `stat`.
//!
//! Design points that make this safe:
//!
//! - **Atomic publish, no lock.** We extract into a unique `.<key>.<pid>.<rand>.tmp`
//!   dir then `rename` it onto `<cache>/runtime-<key>/`. `rename` of a populated
//!   dir is atomic, so a concurrent reader sees the complete dir or nothing. If a
//!   sibling won the race (target already exists) the loser removes its tmp and
//!   uses the winner's dir. No flock, no partial-population window.
//!
//! - **Existence ⟺ integrity.** The dir name embeds the blob's content hash and
//!   only ever appears via the atomic rename, so the dir being present means a
//!   COMPLETE extraction of THIS EXACT blob. That is the integrity sentinel — the
//!   hot path hashes nothing.
//!
//! - **RO-FS fallback.** `~/.cache/nub` first; if it can't be written (immutable
//!   container, read-only `$HOME`) fall back to `$TMPDIR/nub`. The runtime is
//!   needed on every invocation, so a silent `$TMPDIR` fallback keeps nub working
//!   rather than erroring; only when NEITHER is writable do we give up (and log).
//!
//! - **Age-based GC.** After a fresh extract, sibling `runtime-*` dirs older than
//!   30 days are removed (best-effort). Age-based, not "delete all non-current",
//!   so two versions in active use (a global install + an `npx nub@<old>`) don't
//!   evict each other. In-progress `.tmp` dirs and the current dir are never
//!   touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::discovery;

/// The embedded blob: `runtime/` tarred and zstd-19 compressed at build time.
static RUNTIME_BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime.tar.zst"));

/// `runtime-<pkg version>-<blobhash8>` — a compile-time literal baked by build.rs.
const CACHE_KEY: &str = env!("NUB_RUNTIME_CACHE_KEY");

/// Stale-version eviction threshold.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Memoized result of the (at most once) extraction for this process.
static EXTRACTED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Ensure the embedded runtime is extracted and return the dir holding
/// `preload.mjs` / `addons/` / `node_modules/`. Runs the work at most once per
/// process (OnceLock), returning a cheap clone afterward. `None` only on a
/// genuinely unusable environment (no writable cache dir) — the caller then runs
/// without augmentation, exactly as it would for a not-found sidecar.
pub fn ensure_runtime() -> Option<PathBuf> {
    EXTRACTED.get_or_init(extract_once).clone()
}

fn extract_once() -> Option<PathBuf> {
    let candidates = base_candidates();

    // Fast path: a candidate already holds the extracted dir — one stat, no write.
    // (Both `~/.cache/nub` and the `$TMPDIR` fallback are checked so a machine that
    // extracted to the fallback on a read-only `$HOME` still hits the cache.)
    for base in &candidates {
        let target = base.join(CACHE_KEY);
        if target.is_dir() {
            return Some(target);
        }
    }

    // First run for this blob: extract into the first writable base. `try_extract`
    // probes writability by actually creating its tmp dir, so a read-only primary
    // falls through to `$TMPDIR`.
    for base in &candidates {
        if let Some(dir) = try_extract(base) {
            return Some(dir);
        }
    }

    tracing::warn!(
        "could not extract the nub runtime (no writable cache dir); \
         set XDG_CACHE_HOME to a writable path"
    );
    None
}

/// Candidate cache bases in priority order: `~/.cache/nub` (or `$XDG_CACHE_HOME/nub`)
/// then `$TMPDIR/nub`. Deduplicated so an exotic `TMPDIR == cache_dir` setup
/// doesn't try the same path twice.
fn base_candidates() -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    if let Some(c) = discovery::cache_dir() {
        out.push(c);
    }
    let tmp = std::env::temp_dir().join("nub");
    if !out.contains(&tmp) {
        out.push(tmp);
    }
    out
}

/// Extract the blob into `<base>/runtime-<key>/` via a unique tmp dir + atomic
/// rename. Returns the final dir on success (ours or a concurrent winner's), or
/// `None` if this base is unusable (read-only) or the extraction failed.
fn try_extract(base: &Path) -> Option<PathBuf> {
    // create_dir_all is the writability probe: a read-only base fails here and the
    // caller moves on to the next candidate.
    if fs::create_dir_all(base).is_err() {
        return None;
    }
    let target = base.join(CACHE_KEY);

    let tmp = base.join(format!(
        ".{CACHE_KEY}.{}.{}.tmp",
        std::process::id(),
        rand_suffix()
    ));
    // A leftover tmp from a crashed run with the same name is vanishingly unlikely
    // (pid + monotonic-ish rand), but clear it so create + unpack start clean.
    let _ = fs::remove_dir_all(&tmp);
    if fs::create_dir_all(&tmp).is_err() {
        return None;
    }

    if let Err(e) = unpack_blob(&tmp) {
        let _ = fs::remove_dir_all(&tmp);
        tracing::warn!("failed to inflate the embedded nub runtime: {e}");
        return None;
    }

    match fs::rename(&tmp, &target) {
        Ok(()) => {
            gc_stale(base, &target);
            Some(target)
        }
        Err(_) => {
            // Either a concurrent extractor already published `target` (the common,
            // benign case — `rename` onto a populated dir fails on both Unix and
            // Windows), or a genuine FS error. Clean up our tmp and adopt the
            // winner's dir if it materialized.
            let _ = fs::remove_dir_all(&tmp);
            if target.is_dir() { Some(target) } else { None }
        }
    }
}

/// Stream-decompress the embedded zstd blob and unpack the tar into `dest`. The
/// tar entries are at the root (`preload.mjs`, `addons/…`, `node_modules/…`), so
/// they land directly in `dest`, reproducing the sidecar layout.
fn unpack_blob(dest: &Path) -> std::io::Result<()> {
    let decoder = ruzstd::decoding::StreamingDecoder::new(RUNTIME_BLOB)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut archive = tar::Archive::new(decoder);
    // The extracted runtime has no executables (the `.node` is dlopen'd, not
    // exec'd), so preserving the tar-recorded modes (read perms) is sufficient.
    archive.unpack(dest)
}

/// Remove sibling `runtime-*` dirs older than [`MAX_AGE`]. Best-effort, never
/// throws, and never touches the current dir or any in-progress `.tmp` dir (those
/// start with `.`, so the `runtime-` prefix check skips them). Runs only on the
/// rare fresh-extract path.
fn gc_stale(base: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == *current {
            continue;
        }
        if !entry.file_name().to_string_lossy().starts_with("runtime-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|age| age > MAX_AGE)
            .unwrap_or(false);
        if stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// A short, collision-resistant suffix for the tmp dir name — dep-free
/// (SystemTime nanos XOR'd with a per-process atomic counter). It only needs to be
/// unique among this machine's concurrent extractors; the atomic guards two
/// same-process extractors and the nanos guard cross-process ones.
fn rand_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos
        ^ (COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Build a tiny zstd-19 tar blob matching the real embedded layout, so the
    /// unpack/rename/idempotence/race/GC logic can be exercised without the
    /// feature's build.rs output. Mirrors `unpack_blob`'s decode side.
    fn make_test_blob() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let preload = b"// preload\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(preload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "preload.mjs", &preload[..])
            .unwrap();

        let addon = b"\x7fELF-fake-addon";
        let mut h2 = tar::Header::new_gnu();
        h2.set_size(addon.len() as u64);
        h2.set_mode(0o644);
        h2.set_cksum();
        builder
            .append_data(&mut h2, "addons/nub-native.node", &addon[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        zstd::encode_all(&tar_bytes[..], 19).unwrap()
    }

    fn unpack_test_blob(blob: &[u8], dest: &Path) {
        let decoder = ruzstd::decoding::StreamingDecoder::new(blob).unwrap();
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest).unwrap();
    }

    #[test]
    fn blob_roundtrips_to_the_sidecar_layout() {
        let tmp = std::env::temp_dir().join(format!("nub-rtc-rt-{}", rand_suffix()));
        let _ = fs::remove_dir_all(&tmp);
        let blob = make_test_blob();
        unpack_test_blob(&blob, &tmp);

        let mut preload = String::new();
        fs::File::open(tmp.join("preload.mjs"))
            .unwrap()
            .read_to_string(&mut preload)
            .unwrap();
        assert_eq!(preload, "// preload\n");
        assert!(tmp.join("addons/nub-native.node").is_file());
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn extract_then_atomic_rename_is_idempotent() {
        // First extract publishes the dir; a second pass over the same base + key
        // sees it present and reuses it byte-for-byte (no re-write).
        let base = std::env::temp_dir().join(format!("nub-rtc-idem-{}", rand_suffix()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let key = "runtime-test-deadbeef";
        let blob = make_test_blob();

        let publish = |base: &Path| -> PathBuf {
            let target = base.join(key);
            if target.is_dir() {
                return target;
            }
            let tmp = base.join(format!(".{key}.{}.tmp", rand_suffix()));
            fs::create_dir_all(&tmp).unwrap();
            unpack_test_blob(&blob, &tmp);
            match fs::rename(&tmp, &target) {
                Ok(()) => target,
                Err(_) => {
                    let _ = fs::remove_dir_all(&tmp);
                    target
                }
            }
        };

        let a = publish(&base);
        let mtime_a = fs::metadata(a.join("preload.mjs"))
            .unwrap()
            .modified()
            .unwrap();
        let b = publish(&base);
        let mtime_b = fs::metadata(b.join("preload.mjs"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(mtime_a, mtime_b, "second pass must not re-extract");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn read_only_base_create_probe_fails_cleanly() {
        // `try_extract`'s writability probe is `create_dir_all(base)`. Point it at a
        // path whose parent is a FILE (so create_dir_all can't succeed) and confirm
        // the probe fails rather than panicking — the production fallback to $TMPDIR
        // rides on exactly this `is_err()`.
        let file = std::env::temp_dir().join(format!("nub-rtc-file-{}", rand_suffix()));
        fs::write(&file, b"x").unwrap();
        let unusable = file.join("subdir"); // parent is a file → create_dir_all errors
        assert!(fs::create_dir_all(&unusable).is_err());
        fs::remove_file(&file).unwrap();
    }

    #[test]
    fn gc_evicts_stale_keeps_current_and_tmp() {
        let base = std::env::temp_dir().join(format!("nub-rtc-gc-{}", rand_suffix()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let current = base.join("runtime-cur");
        let stale = base.join("runtime-old");
        let tmp = base.join(".runtime-old.123.tmp");
        for d in [&current, &stale, &tmp] {
            fs::create_dir_all(d).unwrap();
        }
        // Backdate the stale dir well past MAX_AGE.
        let old = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
        filetime_set(&stale, old);

        gc_stale(&base, &current);

        assert!(current.is_dir(), "current version must survive GC");
        assert!(!stale.is_dir(), "a >30d sibling must be evicted");
        assert!(
            tmp.is_dir(),
            "an in-progress .tmp dir must never be touched"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    /// Set a dir's mtime via libc `utimes` (unix) — dep-free. On platforms where
    /// this isn't wired the GC age-test is skipped by leaving mtime as-is, which
    /// would make the eviction assertion fail loudly rather than silently pass, so
    /// keep it unix-gated.
    #[cfg(unix)]
    fn filetime_set(path: &Path, time: SystemTime) {
        use std::os::unix::ffi::OsStrExt;
        let secs = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as libc::time_t;
        let tv = libc::timeval {
            tv_sec: secs,
            tv_usec: 0,
        };
        let times = [tv, tv];
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        unsafe {
            libc::utimes(c.as_ptr(), times.as_ptr());
        }
    }

    #[cfg(not(unix))]
    fn filetime_set(_path: &Path, _time: SystemTime) {}
}
