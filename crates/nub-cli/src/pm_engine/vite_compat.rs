//! Vite symlink-GVS serving compat (issue #315).
//!
//! Under nub's default global virtual store a package's realpath is the
//! machine-global store (`~/.cache/nub/pm/virtual-store/…`), OUTSIDE the project
//! root. Vite's dev server realpath-checks every `/@fs`-served module against
//! `server.fs.allow` (default `[workspaceRoot]`), so a store-resident dep served
//! raw (framework client entries, dev-toolbar modules, SSR/optimizeDeps-excluded
//! deps) is rejected `403 … outside of Vite serving allow list`. pnpm never hits
//! this — its virtual store is project-local (`node_modules/.pnpm`, under the
//! workspace root). nub's external store needs Vite told about it.
//!
//! ONE mechanism, PM-agnostic, works-without-nub, zero lock-in — two units:
//!
//! - **Unit A (all Vite versions): write `node_modules/.modules.yaml`.** JSON
//!   `{"virtualStoreDir":"<abs store>"}`. Vite ≥ 8.1 reads it natively
//!   (`server/index.ts`, PR vitejs/vite#22415) and pushes the path onto
//!   `fs.allow`. Additive (nub's own state lives in `.aube-state`), idempotent,
//!   plain data — read regardless of whether nub is in the process.
//!
//! - **Unit B (Vite < 8.1): backport Vite's own 8.1 sniff.** The sniff predates
//!   the majority of installed Vite, so for < 8.1 nub disk-materializes just the
//!   `vite` package project-local (the linker's `diskMaterializePackages` path —
//!   the shared CAS store stays pristine, only the local ejected copy is touched)
//!   and codegen-inserts the sniff at Vite's own `fs.allow`-default computation
//!   site in the bundled (non-minified) dist. That site is upstream of
//!   `createServer`, so it covers a bare `vite dev` CLI as well as
//!   library-embedded Vite (Astro/SvelteKit/Nuxt). The inserted sniff is
//!   YAML-tolerant (JSON first, block-YAML regex fallback) and reads whatever
//!   `virtualStoreDir` any PM wrote — nub's store path lives ONLY in the
//!   `.modules.yaml` data file, never hardcoded into the patch.
//!
//! Both units are gated on `vite` being in the installed graph and on the
//! `NUB_VITE_COMPAT` opt-out (a falsey value disables both). The materialization
//! decision lives in [`super::mod`]'s setting defaults; this module writes the
//! file and patches the ejected copy post-install. Fail-open throughout: a
//! missing anchor / unwritable copy is a no-op, never a corrupt Vite.

use std::path::{Path, PathBuf};

/// User-facing opt-out. Default-on (nub's goal is Vite just-working under the
/// global store); a falsey value disables BOTH units. `NUB_*` is a sanctioned PM
/// knob. Read at the setting-defaults site (materialize decision) and here (the
/// post-install writer/patcher) so the two stay in lockstep.
pub(crate) const VITE_COMPAT_ENV: &str = "NUB_VITE_COMPAT";

/// Whether the Vite compat behavior is enabled. Disabled only by an explicit
/// falsey `NUB_VITE_COMPAT` (`0`/`false`/`no`/`off`, case-insensitive); unset or
/// any other value keeps it on. Same spelling as nub's other positive-default
/// knobs (`NUB_SELF_SHIM`).
pub(crate) fn enabled() -> bool {
    match std::env::var(VITE_COMPAT_ENV) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Post-install entry: write `.modules.yaml` for every install that has Vite
/// anywhere in the graph, and patch each project-local (ejected) Vite < 8.1
/// copy. `root` is the install/workspace root (where the hoisted/GVS
/// `node_modules` lives). Best-effort — a successful install must never fail on
/// a compat step, so every fallible step degrades to a no-op.
///
/// Vite reaches the graph two ways, and both are handled: as a DIRECT dep (a raw
/// `vite` app / a `vite dev` CLI project — the top-level `node_modules/vite`),
/// and TRANSITIVELY as a framework's embedded engine (Astro/SvelteKit/VitePress
/// — the `.nub/vite@*` isolated-store entries, no top-level symlink). A
/// library-embedded framework is itself store-resident and loads ITS Vite via a
/// store-to-store sibling symlink (the shared virtual-store copy, NOT the
/// project-local ejected copy), so the dist backport cannot reach it CAS-safely
/// — but Unit A (`.modules.yaml`) fixes it for Vite ≥ 8.1 (the framework's
/// store Vite reads the file natively). Direct-dep Vite IS loaded from the
/// disk-materialized project-local copy, so the < 8.1 backport reaches it.
pub(crate) fn apply(root: &Path) {
    if !enabled() {
        return;
    }
    let node_modules = root.join("node_modules");
    let copies = discover_vite(&node_modules);
    if copies.is_empty() {
        return; // no Vite in the graph — nothing to do
    }
    let Some(store) = global_virtual_store_dir() else {
        return;
    };
    // Unit A — Vite present anywhere ⇒ write `.modules.yaml` at the workspace
    // root's node_modules (the path Vite resolves via `searchForWorkspaceRoot`).
    // Covers the native sniff (≥ 8.1) for both direct and library-embedded Vite,
    // and feeds the < 8.1 backport. Canonicalize the store so the allow-list
    // path already has any `~/.cache` symlinks resolved, matching Vite's
    // realpath'd module ids.
    let store = std::fs::canonicalize(&store).unwrap_or(store);
    write_modules_yaml(&node_modules, &store);

    // Unit B — patch only project-local (ejected) copies below 8.1. The store
    // copies a framework loads via sibling symlink are shared across projects, so
    // patching them would corrupt the CAS and leak across projects — left to
    // Unit A (≥ 8.1) / docs (< 8.1 library-embedded, the documented gap).
    for c in &copies {
        if c.project_local && vite_lt_8_1(&c.version) {
            patch_vite_dist(&c.dir);
        }
    }
}

/// One resolved Vite package present in the install.
struct ViteCopy {
    /// Canonical (realpath) package dir — the `dist/node` patch target.
    dir: PathBuf,
    version: String,
    /// Whether the realpath lives inside the project (an ejected, patch-safe
    /// copy) vs. the shared machine-global store (never patched).
    project_local: bool,
}

/// Enumerate every distinct Vite package in the install: the top-level
/// `node_modules/vite` (direct dep) plus each `node_modules/.nub/vite@*`
/// isolated-store entry (transitive/library-embedded). Deduplicated by realpath;
/// each entry carries its version and whether it is project-local.
fn discover_vite(node_modules: &Path) -> Vec<ViteCopy> {
    let project_root = node_modules.parent().map(Path::to_path_buf);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    consider_vite(
        &node_modules.join("vite"),
        &project_root,
        &mut seen,
        &mut out,
    );
    if let Ok(entries) = std::fs::read_dir(node_modules.join(".nub")) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("vite@") {
                let pkg = e.path().join("node_modules").join("vite");
                consider_vite(&pkg, &project_root, &mut seen, &mut out);
            }
        }
    }
    out
}

fn consider_vite(
    pkg_dir: &Path,
    project_root: &Option<PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
    out: &mut Vec<ViteCopy>,
) {
    let Ok(real) = std::fs::canonicalize(pkg_dir) else {
        return;
    };
    if !seen.insert(real.clone()) {
        return;
    }
    let Some(version) = read_vite_version(&real) else {
        return;
    };
    let project_local = project_root.as_ref().is_some_and(|r| real.starts_with(r));
    out.push(ViteCopy {
        dir: real,
        version,
        project_local,
    });
}

/// Read a Vite package dir's `version`. Minimal string extraction — avoids a
/// serde_json round-trip; the field is a plain quoted string in every published
/// vite manifest.
fn read_vite_version(pkg_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let key = raw.find("\"version\"")?;
    let after = &raw[key + "\"version\"".len()..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_string())
}

/// Whether the project manifest at `root` declares `vite` as a DIRECT dependency
/// (any of dependencies / devDependencies / optionalDependencies). Drives the
/// disk-materialize decision: only a direct-dep Vite is loaded from the ejected
/// project-local copy the backport patches, so ejecting for a library-embedded
/// Vite (which loads its store copy) would be wasted dedup. Best-effort — an
/// unreadable/absent manifest ⇒ `false`.
pub(crate) fn manifest_declares_vite(root: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(root.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|field| json.get(field).and_then(|v| v.get("vite")).is_some())
}

/// Whether a semver version string is below 8.1.0 (the floor at which Vite's
/// native `.modules.yaml` sniff exists). Parses only major.minor; a malformed
/// version conservatively returns `false` (assume modern → skip the patch, Unit
/// A still covers it). `pub(crate)` so the selective-subtree closure policy
/// ([`super::phantom_closure`]) can auto-detect an embedded vite<8.1 seed with
/// the identical version rule.
pub(crate) fn vite_lt_8_1(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut it = core.split('.');
    let (Some(major), minor) = (it.next(), it.next()) else {
        return false;
    };
    let Ok(major) = major.parse::<u32>() else {
        return false;
    };
    if major != 8 {
        return major < 8;
    }
    minor.and_then(|m| m.parse::<u32>().ok()).unwrap_or(0) < 1
}

/// nub's global virtual-store directory (`<cache>/virtual-store`, where `<cache>`
/// is embedder-namespaced to `~/.cache/nub/pm`). This is the realpath prefix of
/// every store-resident served module, so it is the value Vite must allow. The
/// embedder profile is registered by the time install runs, so
/// `aube_store::dirs::cache_dir()` resolves the nub namespace.
fn global_virtual_store_dir() -> Option<PathBuf> {
    aube_store::dirs::cache_dir().map(|c| c.join(aube_store::VIRTUAL_STORE_SUBDIR))
}

/// Unit A. Write `<node_modules>/.modules.yaml` as JSON `{"virtualStoreDir":…}`.
/// MUST be JSON-flow (Vite ≤ 8.1.x's native sniff parses with `JSON.parse`;
/// block YAML would throw and skip the store). JSON-flow is also valid YAML, so
/// it survives a future YAML-parser upstream fix and the backported regex
/// fallback. Best-effort.
///
/// `.modules.yaml` is also pnpm's OWN install-state file (block YAML, many keys:
/// `hoistPattern`, `packageManager`, …). nub must not clobber a real pnpm state
/// file — so write ONLY when the file is absent or is already nub's own
/// single-key JSON stub. A foreign (pnpm) state file is left untouched (that
/// project's Vite compat is forgone rather than corrupt pnpm's round-trip); the
/// common nub-identity case has no such file and writes freely.
fn write_modules_yaml(node_modules: &Path, store: &Path) {
    if !node_modules.is_dir() {
        return;
    }
    let path = node_modules.join(".modules.yaml");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !is_nub_modules_yaml(&existing) {
            return; // foreign (pnpm) state file — never clobber
        }
    }
    // JSON string-escape the path (Windows backslashes, spaces). serde_json is a
    // nub-cli dep already.
    let value = serde_json::to_string(&store.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "\"\"".to_string());
    let body = format!("{{\"virtualStoreDir\":{value}}}\n");
    let _ = std::fs::write(&path, body);
}

/// Whether a `.modules.yaml` body is nub's own stub (a JSON object whose only
/// key is `virtualStoreDir`) rather than a foreign PM's richer state file. A
/// pnpm state file is block YAML / carries other keys, so it fails to parse as a
/// single-key JSON object and is left alone.
fn is_nub_modules_yaml(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.as_object()
                .map(|o| o.len() == 1 && o.contains_key("virtualStoreDir"))
        })
        .unwrap_or(false)
}

// ───────────────────────── Unit B: the < 8.1 dist backport ─────────────────

/// The bindings Vite's own fs/path are hash-suffixed per build (`path$b`,
/// `fs__default`), so the insert cannot reference them — it brings its own,
/// prepended under mangled names that cannot collide.
const PREPEND: &str = "import{readFileSync as __nubRfs}from\"node:fs\";\
import{join as __nubJoin,isAbsolute as __nubIsAbs,resolve as __nubResolve}from\"node:path\";\n";

/// The sniff, inserted immediately after the anchor (where `allowDirs` is a live
/// array and `searchForWorkspaceRoot`/`root` are in scope). YAML-tolerant
/// (JSON.parse → block-YAML regex fallback) and PM-agnostic: appends whatever
/// `virtualStoreDir` any PM wrote. Strictly better than upstream 8.1's
/// JSON-only sniff (which silently no-ops on real pnpm's block YAML).
const INSERT: &str = ";try{const __wr=searchForWorkspaceRoot(root);\
const __c=__nubRfs(__nubJoin(__wr,\"node_modules\",\".modules.yaml\"),\"utf-8\");\
let __v;try{__v=JSON.parse(__c).virtualStoreDir;}\
catch{const __m=__c.match(/^\\s*virtualStoreDir:\\s*(.+?)\\s*$/m);__v=__m&&__m[1].replace(/^['\"]|['\"]$/g,\"\");}\
if(__v){if(__nubIsAbs(__v))allowDirs.push(__v);\
else if(__v.startsWith(\"..\"))allowDirs.push(__nubResolve(__nubJoin(__wr,\"node_modules\"),__v));}}catch{}";

/// The fs.allow-default computation anchors, in Vite's own bundled-but-not-
/// minified source. `let allowDirs = server.fs.allow;` is identical across
/// Vite 6 and 7; the `[searchForWorkspaceRoot(root)]` form is Vite 5's. The
/// insert goes immediately AFTER the anchor. Vite ≥ 8.1 needs no patch (native
/// sniff), so a new major only needs a one-line anchor check — if it ever stops
/// matching, nothing is patched (fail-open; Unit A still covers ≥ 8.1).
const ANCHORS: &[&str] = &[
    "let allowDirs = server.fs.allow;",            // Vite 6 & 7
    "allowDirs = [searchForWorkspaceRoot(root)];", // Vite 5
];

/// A marker unique to the insert; its presence means a file is already patched,
/// making the whole pass idempotent across re-installs.
const MARKER: &str = "__nubRfs(__nubJoin";

/// Patch an ejected, project-local Vite package's `dist/node/**.js` with the
/// backported sniff. `vite_dir` is the CANONICAL package dir (from
/// [`discover_vite`], already classified project-local). A defensive CAS-safety
/// re-check refuses anything under the shared global store — patching there
/// would corrupt the store shared across every project. Fail-open at every step.
fn patch_vite_dist(vite_dir: &Path) {
    if let Some(store) = global_virtual_store_dir() {
        let store = std::fs::canonicalize(&store).unwrap_or(store);
        if vite_dir.starts_with(&store) {
            return; // never mutate the shared CAS-backed store
        }
    }
    let dist_node = vite_dir.join("dist").join("node");
    if !dist_node.is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_js(&dist_node, &mut files);
    for f in files {
        patch_one(&f);
    }
}

/// Recursively collect `.js` files under `dir` (Vite's dist chunk filenames are
/// hash-suffixed, so the anchor is scanned for, not a filename hardcoded).
/// Depth-capped defensively.
fn collect_js(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_js_depth(dir, out, 0);
}

fn collect_js_depth(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_js_depth(&path, out, depth + 1),
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "js") => {
                out.push(path);
            }
            _ => {}
        }
    }
}

/// Patch one dist file if it carries an anchor and is not already patched.
/// Prepends the import and inserts the sniff after the first matching anchor.
/// The write is best-effort; a read/anchor miss is a no-op.
///
/// CAS-safety, CRITICAL: the ejected copy's files are created by the linker via
/// `hard_link` on non-macOS same-FS targets (macOS clones), so each dist file
/// SHARES its inode with the content-addressed store blob every project
/// hardlinks to. A truncate-in-place write (`fs::write` = `O_TRUNC`) would edit
/// that shared inode and corrupt the global CAS. So write a fresh sibling file
/// and atomically `rename` over the target — the rename repoints the directory
/// entry to a NEW inode, leaving the CAS-shared blob untouched. This also makes
/// the patch atomic (no truncated/half-written chunk on an interrupted write).
fn patch_one(file: &Path) {
    let Ok(src) = std::fs::read_to_string(file) else {
        return;
    };
    if src.contains(MARKER) {
        return; // already patched
    }
    let Some(anchor) = ANCHORS.iter().find(|a| src.contains(**a)) else {
        return;
    };
    // Insert AFTER the first anchor occurrence, then prepend the import.
    let patched = src.replacen(anchor, &format!("{anchor}{INSERT}"), 1);
    let patched = format!("{PREPEND}{patched}");
    write_breaking_hardlink(file, patched.as_bytes());
}

/// Replace `file`'s contents WITHOUT truncating its (possibly CAS-hardlinked)
/// inode: write a temp sibling in the same directory, then atomically rename it
/// over `file`. Same-dir keeps the rename on one filesystem (atomic); the temp
/// name is process- + path-unique. Best-effort — a failure cleans up the temp
/// and leaves the original untouched.
fn write_breaking_hardlink(file: &Path, bytes: &[u8]) {
    let Some(dir) = file.parent() else {
        return;
    };
    let stem = file.file_name().and_then(|n| n.to_str()).unwrap_or("chunk");
    let tmp = dir.join(format!(".{stem}.nub-{}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_only_on_explicit_falsey() {
        // Default-on when unset; the test mutates process env, so it is the sole
        // reader in-process (serialized by cargo per-test-binary is not enough —
        // keep the assertions in one test to avoid cross-test env races).
        let key = VITE_COMPAT_ENV;
        // SAFETY: single-threaded test body; restored at the end.
        unsafe { std::env::remove_var(key) };
        assert!(enabled(), "unset ⇒ on");
        for falsey in ["0", "false", "FALSE", "no", "Off", " off "] {
            unsafe { std::env::set_var(key, falsey) };
            assert!(!enabled(), "{falsey:?} ⇒ off");
        }
        for truthy in ["1", "true", "yes", "anything"] {
            unsafe { std::env::set_var(key, truthy) };
            assert!(enabled(), "{truthy:?} ⇒ on");
        }
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn version_floor_is_8_1_0() {
        for v in ["5.4.21", "6.4.3", "7.3.6", "8.0.9", "8.0.0", "0.4.0"] {
            assert!(vite_lt_8_1(v), "{v} is < 8.1");
        }
        // 8.1.0-beta.0 is where the native sniff landed — its core is 8.1.0, so
        // it (and any 8.1 prerelease) reads as >= 8.1 and needs no patch.
        for v in ["8.1.0", "8.1.3", "8.2.0", "9.0.0", "10.1.1", "8.1.0-beta.0"] {
            assert!(!vite_lt_8_1(v), "{v} is >= 8.1");
        }
        // Malformed ⇒ treat as modern (skip patch; Unit A still applies).
        assert!(!vite_lt_8_1("not-a-version"));
    }

    #[test]
    fn version_extraction_from_pkg_dir() {
        let dir = std::env::temp_dir().join(format!("nub-vite-ver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"vite","version":"7.3.6","type":"module"}"#,
        )
        .unwrap();
        assert_eq!(read_vite_version(&dir).as_deref(), Some("7.3.6"));
        assert_eq!(read_vite_version(&dir.join("nope")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_nub_shaped_modules_yaml_is_overwritable() {
        // nub's own stub — overwritable.
        assert!(is_nub_modules_yaml(r#"{"virtualStoreDir":"/abs"}"#));
        assert!(is_nub_modules_yaml("{\"virtualStoreDir\":\"/abs\"}\n"));
        // pnpm's real state file (block YAML, many keys) — never clobber.
        assert!(!is_nub_modules_yaml(
            "hoistPattern:\n  - '*'\nvirtualStoreDir: node_modules/.pnpm\npackageManager: pnpm@9\n"
        ));
        // A JSON object with extra keys is foreign too (not the single-key stub).
        assert!(!is_nub_modules_yaml(
            r#"{"virtualStoreDir":"/abs","hoistPattern":["*"]}"#
        ));
        assert!(!is_nub_modules_yaml("not json at all"));
    }

    /// write_modules_yaml refuses to clobber a foreign (pnpm) state file but
    /// freely writes when absent or over its own stub.
    #[test]
    fn write_modules_yaml_preserves_foreign_state() {
        let nm = std::env::temp_dir().join(format!("nub-vite-my-{}", std::process::id()));
        std::fs::create_dir_all(&nm).unwrap();
        let f = nm.join(".modules.yaml");
        let store = Path::new("/store/virtual-store");

        // absent → writes nub stub
        write_modules_yaml(&nm, store);
        assert!(is_nub_modules_yaml(&std::fs::read_to_string(&f).unwrap()));

        // over its own stub → rewrites
        write_modules_yaml(&nm, Path::new("/store2"));
        assert!(std::fs::read_to_string(&f).unwrap().contains("/store2"));

        // foreign pnpm file → left untouched
        let pnpm = "hoistPattern:\n  - '*'\nvirtualStoreDir: node_modules/.pnpm\n";
        std::fs::write(&f, pnpm).unwrap();
        write_modules_yaml(&nm, store);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), pnpm);

        let _ = std::fs::remove_dir_all(&nm);
    }

    /// CAS-safety regression guard: the ejected dist files are HARDLINKS to the
    /// content-addressed store on Linux, so the patch write MUST NOT truncate the
    /// shared inode in place. This proves the write severs the link — the sibling
    /// hardlink keeps its original bytes. Fails loudly if `patch_one` ever reverts
    /// to `fs::write` (which is invisible on macOS's copy-on-write link strategy).
    #[test]
    fn patch_write_does_not_mutate_a_hardlinked_sibling() {
        let dir = std::env::temp_dir().join(format!("nub-vite-hl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cas = dir.join("cas-blob.js"); // stands in for the shared store blob
        let ejected = dir.join("ejected.js"); // the project-local hardlink to it
        let original = "let allowDirs = server.fs.allow;\n";
        std::fs::write(&cas, original).unwrap();
        std::fs::hard_link(&cas, &ejected).unwrap();

        write_breaking_hardlink(&ejected, b"PATCHED");

        assert_eq!(std::fs::read_to_string(&ejected).unwrap(), "PATCHED");
        assert_eq!(
            std::fs::read_to_string(&cas).unwrap(),
            original,
            "the CAS-shared inode must be untouched — the write broke the hardlink"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_declares_vite_scans_all_dep_fields() {
        let dir = std::env::temp_dir().join(format!("nub-vite-decl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |body: &str| std::fs::write(dir.join("package.json"), body).unwrap();

        write(r#"{"devDependencies":{"vite":"^7"}}"#);
        assert!(manifest_declares_vite(&dir), "devDependencies");
        write(r#"{"dependencies":{"vite":"7"}}"#);
        assert!(manifest_declares_vite(&dir), "dependencies");
        // Library-embedded: framework declared, Vite only transitive ⇒ not direct.
        write(r#"{"dependencies":{"astro":"^7","@astrojs/react":"^4"}}"#);
        assert!(
            !manifest_declares_vite(&dir),
            "transitive vite is not a direct dep"
        );
        // No manifest ⇒ false, no panic.
        std::fs::remove_file(dir.join("package.json")).unwrap();
        assert!(!manifest_declares_vite(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The codegen patch is anchor-driven, idempotent, and inserts after the
    /// anchor with the import prepended. Exercised on a synthetic chunk carrying
    /// the Vite-6/7 anchor.
    #[test]
    fn patch_is_anchored_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("nub-vite-patch-{}", std::process::id()));
        let dn = dir.join("dist").join("node").join("chunks");
        std::fs::create_dir_all(&dn).unwrap();
        let chunk = dn.join("config.js");
        std::fs::write(
            &chunk,
            "import x from 'y';\nlet allowDirs = server.fs.allow;\nif (x) {}\n",
        )
        .unwrap();

        patch_one(&chunk);
        let after = std::fs::read_to_string(&chunk).unwrap();
        assert!(after.starts_with(PREPEND), "import prepended");
        assert!(after.contains(MARKER), "sniff inserted");
        assert!(
            after.contains("let allowDirs = server.fs.allow;;try{"),
            "insert lands immediately after the anchor"
        );

        patch_one(&chunk);
        let twice = std::fs::read_to_string(&chunk).unwrap();
        assert_eq!(after, twice, "second pass is a no-op (marker guard)");
        assert_eq!(twice.matches(MARKER).count(), 1, "no double insert");

        // A file with no anchor is untouched.
        let plain = dn.join("plain.js");
        std::fs::write(&plain, "export const a = 1;\n").unwrap();
        patch_one(&plain);
        assert_eq!(
            std::fs::read_to_string(&plain).unwrap(),
            "export const a = 1;\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
