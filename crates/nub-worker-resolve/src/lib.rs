//! Pure path helpers for the Worker string-specifier rewrite (`nub-native`'s
//! `worker_rewrite` pass). Factored out of the cdylib so the portability-critical
//! invariants are unit-testable on every platform — including the Windows
//! separator / drive-letter behavior the Linux-only e2e suite cannot reach.
//!
//! THE SOUNDNESS MOAT: [`relativize_for_url`] returns `None` whenever no relative
//! path exists (cross-root / cross-drive). The rewrite pass treats `None` as
//! "leave the node untouched" — it NEVER bakes an absolute build-machine path into
//! the emitted `new URL(...)`, which is the exact portability failure the feature
//! exists to avoid.

use std::path::{Component, Path, PathBuf};

/// Is `specifier` a BARE package name (not relative, not absolute, not a URL
/// scheme, not a `#`-subpath)? The rewrite pass uses this to decide the
/// `import.meta.resolve` emit when nub's TS resolver returned `None` (= Node owns
/// it: node_modules / `exports`). A scheme is `<word>:` at the head (`file:`,
/// `data:`, `node:`, `http:`); a Windows drive (`C:\`) is NOT a scheme — it is
/// absolute, hence not bare.
pub fn is_bare_specifier(specifier: &str) -> bool {
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return false;
    }
    if specifier.starts_with('/') || Path::new(specifier).is_absolute() {
        return false;
    }
    if specifier.starts_with('#') {
        return false;
    }
    !has_url_scheme(specifier)
}

/// A leading `<scheme>:` where scheme is `[A-Za-z][A-Za-z0-9+.-]*` and longer than
/// one char (so a Windows drive letter `C:` is NOT mistaken for a scheme).
fn has_url_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    if colon < 2 {
        return false; // single-letter prefix ⇒ a drive letter, not a scheme
    }
    let mut chars = s[..colon].chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// Re-relativize an absolute resolved `target` back to `from_dir` (the compiling
/// module's directory) → a POSIX-separator relative URL specifier, prefixed with
/// `./` or `../` and percent-encoded for use as the first arg of
/// `new URL(<rel>, import.meta.url)`.
///
/// `None` when no relative path exists (different root/drive, or `target == dir`)
/// — the caller LEAVES THE NODE UNTOUCHED, never emitting an absolute path.
pub fn relativize_for_url(from_dir: &str, target: &str) -> Option<String> {
    let rel = relative_path(Path::new(from_dir), Path::new(target))?;
    // POSIX `/` separators in the emitted URL (a backslash is not a URL path sep).
    let posix = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if posix.is_empty() {
        return None;
    }
    let encoded = encode_url_path(&posix);
    Some(if encoded.starts_with("../") {
        encoded
    } else {
        format!("./{encoded}")
    })
}

/// Pure lexical relative path from `base` (a directory) to `target`, à la Node's
/// `path.relative`. Both must share a root (prefix/drive); `None` otherwise — the
/// cross-root guard. Operates on already-normalized absolute paths.
fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let base_comps: Vec<Component> = base.components().collect();
    let target_comps: Vec<Component> = target.components().collect();

    // Roots must match (same prefix on Windows, same `/` on unix). A differing
    // RootDir/Prefix ⇒ no relative path exists.
    let is_root = |c: &Component| matches!(c, Component::Prefix(_) | Component::RootDir);
    let base_root: Vec<&Component> = base_comps.iter().take_while(|c| is_root(c)).collect();
    let target_root: Vec<&Component> = target_comps.iter().take_while(|c| is_root(c)).collect();
    if base_root != target_root {
        return None;
    }

    let common = base_comps
        .iter()
        .zip(&target_comps)
        .take_while(|(a, b)| a == b)
        .count();

    let mut rel = PathBuf::new();
    for _ in common..base_comps.len() {
        rel.push("..");
    }
    for comp in &target_comps[common..] {
        rel.push(comp.as_os_str());
    }
    Some(rel)
}

/// Percent-encode a path for use as the first arg of `new URL(<literal>, base)`.
/// Encodes, per BYTE of the UTF-8 encoding: the URL-hazard ASCII chars (space,
/// `?`, `#`, `%`) AND every NON-ASCII byte. The non-ASCII case is load-bearing:
/// `é` is `0xC3 0xA9` in UTF-8, so emitting it raw (or worse, byte-as-`char`,
/// which yields latin-1 mojibake `Ã©`) produces a literal that `new URL` decodes
/// to the wrong path. Percent-encoding the UTF-8 bytes round-trips: `new URL`
/// re-decodes `%C3%A9` back to `é`, and `fileURLToPath` recovers the real file.
/// Path separators (`/`) and unreserved ASCII (`.`, `-`, `_`, alnum) pass through.
fn encode_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'/' => out.push('/'),
            // Unreserved ASCII (RFC 3986) + the path-safe `.`/`-`/`_`/`~` — verbatim.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                out.push(b as char)
            }
            // Everything else — URL-hazard ASCII (space/`?`/`#`/`%`/…) AND every
            // non-ASCII UTF-8 byte — is percent-encoded.
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_bare_vs_relative_vs_absolute() {
        assert!(is_bare_specifier("pkg"));
        assert!(is_bare_specifier("@scope/pkg/worker"));
        assert!(!is_bare_specifier("./w.ts"));
        assert!(!is_bare_specifier("../lib/w"));
        assert!(!is_bare_specifier("/abs/w.js"));
        assert!(!is_bare_specifier("file:///w.js"));
        assert!(!is_bare_specifier("data:text/javascript,0"));
        assert!(!is_bare_specifier("#internal/w"));
    }

    #[test]
    fn drive_letter_is_not_a_scheme() {
        // A Windows drive prefix is absolute, never bare; `C:` must not parse as a
        // URL scheme (which would mis-route it through `import.meta.resolve`).
        assert!(!has_url_scheme("C:\\proj\\w.ts"));
        assert!(has_url_scheme("file:///x"));
        assert!(has_url_scheme("data:text/js,0"));
    }

    #[test]
    fn relativizes_to_posix_dot_prefixed() {
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/src/w.ts").unwrap(),
            "./w.ts"
        );
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/lib/w.ts").unwrap(),
            "../lib/w.ts"
        );
    }

    #[test]
    fn percent_encodes_spaces() {
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/src/my worker.ts").unwrap(),
            "./my%20worker.ts"
        );
    }

    #[test]
    fn percent_encodes_non_ascii_utf8_bytes() {
        // `é` is 0xC3 0xA9 in UTF-8 — it MUST become `%C3%A9`, never the latin-1
        // mojibake `Ã©` a byte-as-char cast produces. `new URL` re-decodes
        // `%C3%A9` → `é`, so the resolved file path round-trips.
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/src/café.ts").unwrap(),
            "./caf%C3%A9.ts"
        );
        // A non-Latin script (CJK) round-trips the same way.
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/src/worker-日本.ts").unwrap(),
            "./worker-%E6%97%A5%E6%9C%AC.ts"
        );
    }

    #[test]
    fn percent_encodes_url_hazard_ascii() {
        // `?`/`#` would otherwise be parsed by `new URL` as query/fragment; `%` is
        // the escape char itself; a `"` is encoded too (defense in depth — oxc also
        // escapes it in the JS string literal).
        assert_eq!(
            relativize_for_url("/proj/src", "/proj/src/a?b#c.ts").unwrap(),
            "./a%3Fb%23c.ts"
        );
    }

    #[test]
    fn same_path_is_none() {
        // `target == dir` ⇒ empty relative ⇒ None ⇒ caller leaves the node alone.
        assert!(relativize_for_url("/proj/src", "/proj/src").is_none());
    }

    #[test]
    fn output_is_never_absolute() {
        // The portability moat: every Some result is a `./`/`../`-prefixed relative
        // URL — never an absolute path baked in from the build machine.
        for (dir, target) in [
            ("/proj/src", "/proj/src/w.ts"),
            ("/proj/a/b/c", "/proj/x/y/deep.ts"),
            ("/proj/src/feature", "/proj/lib/w.ts"),
        ] {
            let r = relativize_for_url(dir, target).unwrap();
            assert!(
                (r.starts_with("./") || r.starts_with("../")) && !r.starts_with('/'),
                "{r} for ({dir}, {target}) must be a relative URL, never absolute"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn cross_drive_is_none() {
        // Different Windows drives share no root ⇒ no relative path ⇒ the rewrite
        // must NOT fire (would otherwise be forced to bake an absolute path).
        assert!(relativize_for_url("C:\\a\\b", "D:\\a\\b\\w.ts").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_separators_become_posix() {
        let r = relativize_for_url("C:\\proj\\src", "C:\\proj\\src\\w.ts").unwrap();
        assert_eq!(r, "./w.ts");
    }
}
