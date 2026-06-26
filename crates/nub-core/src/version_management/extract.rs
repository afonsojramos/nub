//! Extract a verified Node dist archive into nub's store.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Maximum total decompressed bytes accepted from a single archive — the
/// decompression-bomb ceiling (N2). 1 GiB, mirroring the engine's
/// `aube_store::MAX_TARBALL_DECOMPRESSED_BYTES`. A stock Node dist tarball
/// unpacks to well under 100 MiB, so this sits an order of magnitude above any
/// real artifact while stopping a malicious/mirror-served small-compressed /
/// huge-decompressed payload from exhausting disk or memory. The download is
/// already checksum-verified against `SHASUMS256.txt` before extraction, so this
/// is defense-in-depth against a MITM/mirror that also forged the checksum.
///
/// The cap is wired through the `_capped` extractor variants as a parameter
/// rather than read from this const directly, so a bomb test can inject a tiny
/// cap without a `cfg(test)` value that would also throttle the real-archive
/// (~25 MB Node / ~15 MB pnpm) provisioning e2e tests.
pub(crate) const MAX_ARCHIVE_DECOMPRESSED_BYTES: u64 = 1 << 30;

/// Maximum bytes for a single archive entry (zip per-file cap). 512 MiB, the
/// same shape as `aube_store::MAX_TARBALL_ENTRY_BYTES`.
pub(crate) const MAX_ARCHIVE_ENTRY_BYTES: u64 = 512 << 20;

/// A `Read` wrapper that refuses to deliver more than `remaining` bytes,
/// surfacing exhaustion as an `io::Error` rather than a clean EOF. Mirror of
/// `aube_store`'s `CappedReader`: when it wraps a gzip/xz decoder feeding a tar
/// archive, a clean EOF on a block boundary would let a crafted archive silently
/// truncate into a partial tree; an explicit error keeps the tar iterator from
/// accepting a half-read stream as complete. Shared with [`crate::pm::extract`].
pub(crate) struct CappedReader<R: Read> {
    inner: R,
    remaining: u64,
    cap: u64,
}

impl<R: Read> CappedReader<R> {
    pub(crate) fn new(inner: R, cap: u64) -> Self {
        Self {
            inner,
            remaining: cap,
            cap,
        }
    }
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // A zero-length read is a no-op by the `Read` contract and must not error
        // even once the cap is exhausted.
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("archive decompression exceeds the {}-byte cap", self.cap),
            ));
        }
        let want = buf.len().min(self.remaining as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// The single top-level directory `dest_parent` now holds after an archive was
/// unpacked into it — the `node-v<ver>-<plat>` dir for Node tarballs/zips, the
/// `package` dir for an npm `.tgz`. Errors if zero or more than one dir is present
/// (a stock archive always has exactly one). Shared by every extractor so the
/// "one top dir, or it's malformed" rule lives in one place; `archive` is only
/// used to name the file in error messages.
pub(crate) fn single_top_dir(dest_parent: &Path, archive: &Path) -> Result<PathBuf> {
    let mut top: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dest_parent)? {
        let path = entry?.path();
        if path.is_dir() && top.replace(path).is_some() {
            bail!(
                "expected a single top-level directory in {}",
                archive.display()
            );
        }
    }
    top.with_context(|| format!("no directory extracted from {}", archive.display()))
}

/// Decode a `.tar.xz` and unpack it under `dest_parent`, returning the single
/// top-level directory it created (the `node-v<ver>-<plat>` dir). The `tar` crate
/// guards against path-traversal (`..` / absolute entries) during `unpack`.
pub fn extract_tar_xz(archive: &Path, dest_parent: &Path) -> Result<PathBuf> {
    extract_tar_xz_capped(archive, dest_parent, MAX_ARCHIVE_DECOMPRESSED_BYTES)
}

/// [`extract_tar_xz`] with the decompression cap as an explicit parameter — the
/// seam a bomb test uses to inject a tiny cap (the public entry uses the prod
/// [`MAX_ARCHIVE_DECOMPRESSED_BYTES`]).
fn extract_tar_xz_capped(
    archive: &Path,
    dest_parent: &Path,
    decompressed_cap: u64,
) -> Result<PathBuf> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    // Cap the DECOMPRESSED stream against a decompression bomb (N2). Node
    // tarballs legitimately ship intra-tree symlinks (`bin/npm → ../lib/…`), so
    // unlike the PM-tgz extractor this path keeps `tar::unpack` (which preserves
    // them); the cap is the only hardening here.
    let decoder = CappedReader::new(liblzma::read::XzDecoder::new(file), decompressed_cap);
    let mut tar = tar::Archive::new(decoder);
    std::fs::create_dir_all(dest_parent)
        .with_context(|| format!("create {}", dest_parent.display()))?;
    tar.unpack(dest_parent)
        .with_context(|| format!("extracting {}", archive.display()))?;
    single_top_dir(dest_parent, archive)
}

/// Unpack a Node Windows dist `.zip` under `dest_parent`, returning the single
/// top-level directory it created (the `node-v<ver>-win-<arch>` dir holding
/// `node.exe`). Pure-Rust via the `zip` crate — no shell-out to PowerShell's
/// `Expand-Archive` or `tar.exe`, so extraction is identical across Windows
/// versions and the checksum-verify-then-extract flow stays in one process. The
/// entry walk uses `ZipFile::enclosed_name` as the path-traversal guard (`..` /
/// absolute entries are refused) and caps each entry + the archive total against
/// a zip bomb (N2).
///
/// Mode handling: stored unix mode bits are reapplied on unix targets and are a
/// no-op on Windows (where executability is by extension, so `node.exe` is
/// runnable automatically). Stock Node `.zip`s carry POSIX modes in their extra
/// fields, so a unix build extracting one keeps `node.exe` readable; the normal
/// Windows-only path doesn't depend on it.
pub fn extract_zip(archive: &Path, dest_parent: &Path) -> Result<PathBuf> {
    extract_zip_capped(
        archive,
        dest_parent,
        MAX_ARCHIVE_ENTRY_BYTES,
        MAX_ARCHIVE_DECOMPRESSED_BYTES,
    )
}

/// [`extract_zip`] with the per-entry + archive-total caps as explicit
/// parameters — the seam a zip-bomb test uses to inject tiny caps (the public
/// entry uses the prod constants).
fn extract_zip_capped(
    archive: &Path,
    dest_parent: &Path,
    entry_cap: u64,
    total_cap: u64,
) -> Result<PathBuf> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).with_context(|| format!("reading zip {}", archive.display()))?;
    std::fs::create_dir_all(dest_parent)
        .with_context(|| format!("create {}", dest_parent.display()))?;

    // A manual entry walk (instead of `ZipArchive::extract`) so each entry's
    // decompressed output can be capped against a zip bomb (N2). `enclosed_name`
    // is the path-traversal guard — it returns `None` for any `..`/absolute entry.
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .with_context(|| format!("reading zip entry {i} of {}", archive.display()))?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(
                "zip entry path {:?} escapes the extraction dir in {}",
                entry.name(),
                archive.display()
            );
        };
        let dest_path = dest_parent.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest_path)
                .with_context(|| format!("create {}", dest_path.display()))?;
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&dest_path)
            .with_context(|| format!("create {}", dest_path.display()))?;
        // Per-entry cap (+1 so a write of exactly the cap is detectable) plus a
        // running archive-total cap — a lying header can't amplify past either.
        let written = std::io::copy(&mut (&mut entry).take(entry_cap + 1), &mut out)
            .with_context(|| format!("extracting {} from {}", rel.display(), archive.display()))?;
        if written > entry_cap {
            bail!(
                "zip entry {} exceeds the {entry_cap}-byte per-entry cap",
                rel.display()
            );
        }
        total = total.saturating_add(written);
        if total > total_cap {
            bail!(
                "zip {} exceeds the {total_cap}-byte archive cap",
                archive.display()
            );
        }
        // Preserve stored unix mode bits (Node `.zip`s carry POSIX modes in their
        // extra fields) so a unix host extracting one keeps `node.exe` readable —
        // the behavior `ZipArchive::extract` gave for free. A no-op on Windows,
        // where executability is by extension.
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode));
        }
    }
    single_top_dir(dest_parent, archive)
}

/// Extract `archive` by type: `.tar.xz` (macOS/Linux) or `.zip` (Windows). Both
/// paths verify the archive against its published SHA-256 before this call (see
/// `provision_node`) and unpack in-process — no `tar`/`xz`/`Expand-Archive`
/// shell-out — so the same verify-then-extract guarantee holds on every host.
pub fn extract_archive(archive: &Path, dest_parent: &Path) -> Result<PathBuf> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.xz") {
        extract_tar_xz(archive, dest_parent)
    } else if name.ends_with(".zip") {
        extract_zip(archive, dest_parent)
    } else {
        bail!("unrecognized Node archive format: {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny `.tar.xz` with a single top dir, extract it, and confirm the
    /// returned top dir + a nested file survive. Exercises the real liblzma + tar
    /// decode path without the network.
    #[test]
    fn extract_tar_xz_returns_the_single_top_dir() {
        let dir = std::env::temp_dir().join(format!("nub-xz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("sample.tar.xz");

        // Author the archive: top/bin/node + top/README.
        {
            let file = std::fs::File::create(&archive).unwrap();
            let enc = liblzma::write::XzEncoder::new(file, 6);
            let mut builder = tar::Builder::new(enc);
            let mut header = |path: &str, contents: &[u8]| {
                let mut h = tar::Header::new_gnu();
                h.set_size(contents.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                builder.append_data(&mut h, path, contents).unwrap();
            };
            header("top/bin/node", b"#!/bin/sh\n");
            header("top/README", b"hi\n");
            builder.into_inner().unwrap().finish().unwrap();
        }

        let out = dir.join("extracted");
        let top = extract_tar_xz(&archive, &out).unwrap();
        assert_eq!(top.file_name().unwrap(), "top");
        assert!(top.join("bin").join("node").is_file());
        assert!(top.join("README").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Build a tiny `.zip` with a single top dir + a nested file, extract it, and
    /// confirm the returned top dir + nested file survive. Mirrors the tar test.
    /// The zip format is identical cross-platform, so this proves the extraction
    /// logic on the dev box (macOS) even though the real Windows provisioning e2e
    /// can only run on the windows-latest CI leg.
    #[test]
    fn extract_zip_returns_the_single_top_dir() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nub-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("sample.zip");

        // Author the archive: top/node.exe + top/README — one top-level dir, like
        // a stock node-v<ver>-win-<arch>.zip.
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("top/node.exe", opts).unwrap();
            writer.write_all(b"MZ\x90\x00").unwrap();
            writer.start_file("top/README", opts).unwrap();
            writer.write_all(b"hi\n").unwrap();
            writer.finish().unwrap();
        }

        let out = dir.join("extracted");
        let top = extract_zip(&archive, &out).unwrap();
        assert_eq!(top.file_name().unwrap(), "top");
        assert!(top.join("node.exe").is_file());
        assert!(top.join("README").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// N2: a `.tar.xz` whose decompressed output dwarfs its compressed size — a
    /// MITM/malicious-mirror Node tarball that also forged the checksum. The
    /// `CappedReader` must error before the payload exhausts disk. Drives the
    /// `_capped` seam with a 1 MiB cap (the public entry uses the 1 GiB prod cap,
    /// which the real ~25 MB Node provisioning e2e relies on).
    #[test]
    fn extract_tar_xz_rejects_a_decompression_bomb() {
        let dir = std::env::temp_dir().join(format!("nub-xz-bomb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("bomb.tar.xz");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let enc = liblzma::write::XzEncoder::new(file, 6);
            let mut builder = tar::Builder::new(enc);
            let big = vec![0u8; 4 * 1024 * 1024];
            let mut h = tar::Header::new_gnu();
            h.set_size(big.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            builder
                .append_data(&mut h, "top/big.bin", &big[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        assert!(std::fs::metadata(&archive).unwrap().len() < 64 * 1024);
        let out = dir.join("extracted");
        let err = format!(
            "{:#}",
            extract_tar_xz_capped(&archive, &out, 1 << 20).unwrap_err()
        );
        assert!(
            err.to_lowercase().contains("cap") || err.to_lowercase().contains("decompress"),
            "a decompression bomb must be refused by the cap: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// N2: a `.zip` entry that decompresses past the per-entry cap (a zip bomb).
    /// The capped entry copy must error rather than write the whole payload.
    /// Drives the `_capped` seam with a 1 MiB per-entry cap.
    #[test]
    fn extract_zip_rejects_an_oversized_entry() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nub-zip-bomb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("bomb.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("top/big.bin", opts).unwrap();
            writer.write_all(&vec![0u8; 4 * 1024 * 1024]).unwrap(); // > the 1 MiB injected cap
            writer.finish().unwrap();
        }
        assert!(std::fs::metadata(&archive).unwrap().len() < 64 * 1024);
        let out = dir.join("extracted");
        let err = format!(
            "{:#}",
            extract_zip_capped(&archive, &out, 1 << 20, 1 << 30).unwrap_err()
        );
        assert!(
            err.to_lowercase().contains("cap"),
            "an oversized zip entry must be refused by the cap: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
