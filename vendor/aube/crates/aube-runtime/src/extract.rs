//! Plain extract-to-directory for Node release archives. Not the CAS
//! import path — runtime installs keep Node's native layout so
//! aube-managed and mise-managed installs look identical to
//! discovery.

use crate::error::Error;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Maximum total decompressed bytes accepted from a single runtime archive — the
/// decompression-bomb ceiling. 1 GiB, mirroring
/// `aube_store::MAX_TARBALL_DECOMPRESSED_BYTES`. A stock Node release unpacks to
/// well under 100 MiB, so this sits an order of magnitude above any real artifact
/// while stopping a malicious/mirror-served small-compressed / huge-decompressed
/// payload from exhausting disk or memory. The download is checksum-verified
/// before extraction, so this is defense-in-depth against a forged-checksum MITM.
/// Lowered under `cfg(test)` (as `aube_store` does) so a bomb test stays cheap.
#[cfg(not(test))]
const MAX_ARCHIVE_DECOMPRESSED_BYTES: u64 = 1 << 30;
#[cfg(test)]
const MAX_ARCHIVE_DECOMPRESSED_BYTES: u64 = 1 << 20;

/// Maximum bytes for a single zip entry — the per-file cap on the zip path,
/// matching `aube_store::MAX_TARBALL_ENTRY_BYTES`.
#[cfg(not(test))]
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 512 << 20;
#[cfg(test)]
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 1 << 20;

/// A `Read` wrapper that refuses to deliver more than `remaining` bytes,
/// surfacing exhaustion as an `io::Error` rather than a clean EOF — so a crafted
/// archive can't silently truncate a tar stream into a partial tree at a block
/// boundary. Local mirror of `aube_store`'s `CappedReader` (it is `pub(crate)`
/// there, so it can't be shared across the crate boundary).
struct CappedReader<R: Read> {
    inner: R,
    remaining: u64,
}

impl<R: Read> CappedReader<R> {
    fn new(inner: R, cap: u64) -> Self {
        Self {
            inner,
            remaining: cap,
        }
    }
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "archive decompression exceeds the {MAX_ARCHIVE_DECOMPRESSED_BYTES}-byte cap"
                ),
            ));
        }
        let want = buf.len().min(self.remaining as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Extract `archive_path` into `dest`. `strip_first` drops the
/// top-level directory (`node-v{V}-{slug}/`; aube release archives
/// have no top dir and pass `false`). `zip` selects the Windows zip
/// format; everything else is gzipped tar.
///
/// Runs blocking I/O — call inside `spawn_blocking`.
pub(crate) fn extract_archive(
    archive_path: &Path,
    dest: &Path,
    zip: bool,
    strip_first: bool,
) -> Result<(), Error> {
    if zip {
        extract_zip(archive_path, dest, strip_first)
    } else {
        extract_tar_gz(archive_path, dest, strip_first)
    }
}

fn extract_tar_gz(archive_path: &Path, dest: &Path, strip_first: bool) -> Result<(), Error> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| Error::io(format!("open {}", archive_path.display()), e))?;
    // Cap the DECOMPRESSED stream against a gzip bomb (defense-in-depth on top of
    // the upstream checksum gate).
    let decoder = CappedReader::new(
        flate2::read::GzDecoder::new(std::io::BufReader::new(file)),
        MAX_ARCHIVE_DECOMPRESSED_BYTES,
    );
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(|e| Error::ExtractFailed {
        reason: e.to_string(),
    })? {
        let mut entry = entry.map_err(|e| Error::ExtractFailed {
            reason: e.to_string(),
        })?;
        let path = entry.path().map_err(|e| Error::ExtractFailed {
            reason: e.to_string(),
        })?;
        // `entry_dest_path` doubles as the path-escape guard: it
        // returns None for the top-level dir entry (when stripping)
        // and for any path containing `..` / absolute components.
        let Some(stripped) = entry_dest_path(&path, strip_first) else {
            continue;
        };
        // Node tarballs legitimately contain intra-tree symlinks
        // (`bin/npm → ../lib/node_modules/npm/bin/npm-cli.js`), so
        // symlinks are allowed — but only when the resolved target
        // stays inside `dest`. The checksum gate upstream already
        // authenticates the archive; this is defense in depth.
        if matches!(
            entry.header().entry_type(),
            tar::EntryType::Symlink | tar::EntryType::Link
        ) {
            let target = entry
                .link_name()
                .ok()
                .flatten()
                .ok_or_else(|| Error::ExtractFailed {
                    reason: format!("link entry {} has no target", stripped.display()),
                })?;
            if !link_target_stays_inside(dest, &stripped, &target) {
                return Err(Error::ExtractFailed {
                    reason: format!(
                        "link {} escapes the install dir (target {})",
                        stripped.display(),
                        target.display()
                    ),
                });
            }
        }
        let dest_path = dest.join(&stripped);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("create {}", parent.display()), e))?;
        }
        entry.unpack(&dest_path).map_err(|e| Error::ExtractFailed {
            reason: format!("{}: {e}", stripped.display()),
        })?;
    }
    Ok(())
}

/// Lexically resolve a link target relative to its entry location and
/// check the result stays under `dest`. Absolute targets are rejected
/// outright.
fn link_target_stays_inside(dest: &Path, entry_rel: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let from_dir = match entry_rel.parent() {
        Some(p) => dest.join(p),
        None => dest.to_path_buf(),
    };
    let resolved = aube_util::path::normalize_lexical(&from_dir.join(target));
    resolved.starts_with(dest)
}

fn extract_zip(archive_path: &Path, dest: &Path, strip_first: bool) -> Result<(), Error> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| Error::io(format!("open {}", archive_path.display()), e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::ExtractFailed {
        reason: e.to_string(),
    })?;
    // Running decompressed total, capped per-entry and per-archive against a zip
    // bomb (defense-in-depth on top of the upstream checksum gate).
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| Error::ExtractFailed {
            reason: e.to_string(),
        })?;
        let Some(raw_path) = entry.enclosed_name() else {
            return Err(Error::ExtractFailed {
                reason: format!("unsafe entry path {:?}", entry.name()),
            });
        };
        let Some(stripped) = entry_dest_path(&raw_path, strip_first) else {
            continue;
        };
        let dest_path = dest.join(&stripped);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest_path)
                .map_err(|e| Error::io(format!("create {}", dest_path.display()), e))?;
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("create {}", parent.display()), e))?;
        }
        let mut out = std::fs::File::create(&dest_path)
            .map_err(|e| Error::io(format!("create {}", dest_path.display()), e))?;
        // Per-entry cap (+1 so a write of exactly the cap is detectable) plus the
        // running archive total — a lying header can't amplify past either.
        let written = std::io::copy(
            &mut (&mut entry).take(MAX_ARCHIVE_ENTRY_BYTES + 1),
            &mut out,
        )
        .map_err(|e| Error::ExtractFailed {
            reason: format!("{}: {e}", stripped.display()),
        })?;
        if written > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(Error::ExtractFailed {
                reason: format!(
                    "{} exceeds the {MAX_ARCHIVE_ENTRY_BYTES}-byte per-entry cap",
                    stripped.display()
                ),
            });
        }
        total = total.saturating_add(written);
        if total > MAX_ARCHIVE_DECOMPRESSED_BYTES {
            return Err(Error::ExtractFailed {
                reason: format!(
                    "archive exceeds the {MAX_ARCHIVE_DECOMPRESSED_BYTES}-byte decompression cap"
                ),
            });
        }
    }
    Ok(())
}

/// Compute an entry's destination-relative path, optionally dropping
/// the leading component, and validate it stays relative (no `..`, no
/// absolute components). Returns `None` for the bare top-level dir
/// entry when stripping, and for any unsafe path.
fn entry_dest_path(path: &Path, strip_first: bool) -> Option<PathBuf> {
    let rest: PathBuf = if strip_first {
        let mut components = path.components();
        components.next()?;
        components.as_path().to_path_buf()
    } else {
        path.to_path_buf()
    };
    if rest.as_os_str().is_empty() {
        return None;
    }
    for c in rest.components() {
        match c {
            Component::Normal(_) => {}
            _ => return None,
        }
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_tar_gz(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, path, content.as_bytes())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn tar_strips_top_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = make_tar_gz(&[
            ("node-v22.1.0-linux-x64/bin/node", "fake-binary"),
            ("node-v22.1.0-linux-x64/LICENSE", "mit"),
        ]);
        let archive = tmp.path().join("a.tar.gz");
        std::fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        extract_archive(&archive, &dest, false, true).unwrap();
        assert!(dest.join("bin/node").is_file());
        assert!(dest.join("LICENSE").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn tar_intra_tree_symlink_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "top/lib/real.js", "hi()".as_bytes())
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        builder
            .append_link(&mut link, "top/bin/npm", "../lib/real.js")
            .unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        std::fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        extract_archive(&archive, &dest, false, true).unwrap();
        assert!(dest.join("bin/npm").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("bin/npm")).unwrap(),
            "hi()"
        );
    }

    #[test]
    fn escape_paths_are_rejected_by_strip_guard() {
        // The tar crate refuses to even *author* `..` entries, so the
        // guard is exercised directly: any remainder containing `..`,
        // `.` or absolute components must be dropped.
        assert_eq!(entry_dest_path(Path::new("top/../escape.txt"), true), None);
        assert_eq!(entry_dest_path(Path::new("top/a/../../b"), true), None);
        assert_eq!(
            entry_dest_path(Path::new("top//etc/passwd"), true),
            Some(PathBuf::from("etc/passwd"))
        );
        assert_eq!(entry_dest_path(Path::new("top"), true), None);
        assert_eq!(
            entry_dest_path(Path::new("top/bin/node"), true),
            Some(PathBuf::from("bin/node"))
        );
        // No-strip mode: paths kept verbatim, same escape rules.
        assert_eq!(
            entry_dest_path(Path::new("aube"), false),
            Some(PathBuf::from("aube"))
        );
        assert_eq!(entry_dest_path(Path::new("../aube"), false), None);
    }

    #[test]
    fn symlink_escape_guard() {
        let dest = Path::new("/x/dest");
        assert!(link_target_stays_inside(
            dest,
            Path::new("bin/npm"),
            Path::new("../lib/npm-cli.js")
        ));
        assert!(!link_target_stays_inside(
            dest,
            Path::new("bin/npm"),
            Path::new("../../../etc/passwd")
        ));
        assert!(!link_target_stays_inside(
            dest,
            Path::new("bin/npm"),
            Path::new("/etc/passwd")
        ));
    }

    #[test]
    fn tar_gz_decompression_bomb_is_capped() {
        // A small-compressed / huge-decompressed `.tar.gz` (the gzip-bomb shape)
        // must be refused by the CappedReader before the payload exhausts disk.
        // Test cap is 1 MiB; author well past it from a tiny compressed input.
        let tmp = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::best(),
        ));
        let big = vec![0u8; 4 * 1024 * 1024];
        let mut header = tar::Header::new_gnu();
        header.set_size(big.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "top/big.bin", &big[..])
            .unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        assert!(
            bytes.len() < 64 * 1024,
            "compressed bomb must be tiny: {}",
            bytes.len()
        );
        let archive = tmp.path().join("bomb.tar.gz");
        std::fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // Extraction must be refused, and the cap must have interrupted the write
        // BEFORE the full 4 MiB landed (the tar-crate unpack error wraps the
        // CappedReader's io error without surfacing its message, so assert the
        // truncation behavior, not the wording).
        assert!(
            extract_archive(&archive, &dest, false, true).is_err(),
            "a decompression bomb must be refused"
        );
        if let Ok(meta) = std::fs::metadata(dest.join("big.bin")) {
            assert!(
                meta.len() <= MAX_ARCHIVE_DECOMPRESSED_BYTES,
                "the cap must interrupt the write; any partial file is at most the cap, got {} bytes",
                meta.len()
            );
        }
    }

    #[test]
    fn zip_oversized_entry_is_capped() {
        use std::io::Write;
        // A zip entry decompressing past the per-entry cap (1 MiB under test) must
        // be refused rather than written in full.
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("bomb.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        writer.start_file("top/big.bin", opts).unwrap();
        writer.write_all(&vec![0u8; 4 * 1024 * 1024]).unwrap();
        writer.finish().unwrap();
        assert!(std::fs::metadata(&archive).unwrap().len() < 64 * 1024);
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_archive(&archive, &dest, true, true).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("cap"),
            "an oversized zip entry must be refused by the cap: {msg}"
        );
    }

    #[test]
    fn zip_strips_top_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        writer
            .start_file("node-v22.1.0-win-x64/node.exe", opts)
            .unwrap();
        writer.write_all(b"fake-exe").unwrap();
        writer
            .start_file("node-v22.1.0-win-x64/npm.cmd", opts)
            .unwrap();
        writer.write_all(b"@echo off").unwrap();
        writer.finish().unwrap();

        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        extract_archive(&archive, &dest, true, true).unwrap();
        assert!(dest.join("node.exe").is_file());
        assert!(dest.join("npm.cmd").is_file());
    }
}
