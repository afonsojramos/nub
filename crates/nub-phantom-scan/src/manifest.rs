//! Parse a package's `package.json` into (a) its DECLARED dependency surface —
//! the sets a specifier is checked against — and (b) its published ENTRY POINTS,
//! the roots of the reachable-module walk.
//!
//! The declared surface deliberately mirrors what a consumer install actually
//! makes resolvable at runtime: `dependencies`, `optionalDependencies`, and
//! `peerDependencies` (split by the `peerDependenciesMeta.<x>.optional` flag),
//! plus bundled deps. `devDependencies` are intentionally EXCLUDED — they are not
//! installed for consumers, so an import of one from published code is a phantom.

use std::collections::BTreeSet;

use serde_json::Value;

/// The declared dependency surface a specifier is classified against.
#[derive(Debug, Default)]
pub struct Manifest {
    pub name: String,
    /// `dependencies` ∪ `optionalDependencies` — hard-declared, always resolvable.
    pub deps: BTreeSet<String>,
    /// Required peers (`peerDependencies` without an `optional` meta flag).
    pub required_peers: BTreeSet<String>,
    /// Optional peers (`peerDependenciesMeta.<x>.optional === true`). Declared —
    /// NOT phantoms — but reported as their own category (the pick-your-plugin
    /// pattern that a naive detector over-flags).
    pub optional_peers: BTreeSet<String>,
    /// `bundledDependencies` / `bundleDependencies` — shipped inside the tarball.
    pub bundled: BTreeSet<String>,
    /// Published entry files (relative paths from the package root) — the roots
    /// of the reachable-module walk, each tagged by whether it is the main entry
    /// or a non-`.` `exports` subpath (the adapter surface).
    pub entry_points: Vec<Entry>,
}

/// Which published surface an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `main` / `module` / `bin` / the `exports."."` root.
    Main,
    /// A non-`.` `exports` subpath (`./zod`, `./vitest`) — the adapter surface a
    /// consumer opts into by importing `<pkg>/<subpath>`.
    Subpath,
}

/// A published entry file + its surface kind.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
}

impl Manifest {
    /// Parse from raw `package.json` bytes. Returns `None` if the JSON is
    /// unparseable or has no name (a package with no identity can't be analyzed).
    pub fn parse(raw: &[u8]) -> Option<Manifest> {
        let v: Value = serde_json::from_slice(raw).ok()?;
        let name = v.get("name")?.as_str()?.to_string();

        let mut m = Manifest {
            name: name.clone(),
            ..Default::default()
        };

        collect_keys(&v, "dependencies", &mut m.deps);
        collect_keys(&v, "optionalDependencies", &mut m.deps);
        collect_keys(&v, "peerDependencies", &mut m.required_peers);

        // Move any peer flagged optional out of required_peers into optional_peers.
        if let Some(meta) = v.get("peerDependenciesMeta").and_then(Value::as_object) {
            for (peer, cfg) in meta {
                let optional = cfg
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if optional {
                    m.required_peers.remove(peer);
                    m.optional_peers.insert(peer.clone());
                }
            }
        }

        collect_bundled(&v, &mut m.bundled);
        m.entry_points = collect_entry_points(&v);
        Some(m)
    }
}

fn collect_keys(v: &Value, field: &str, out: &mut BTreeSet<String>) {
    if let Some(obj) = v.get(field).and_then(Value::as_object) {
        for k in obj.keys() {
            out.insert(k.clone());
        }
    }
}

/// `bundledDependencies`/`bundleDependencies` is an ARRAY of names (both spellings
/// are valid npm).
fn collect_bundled(v: &Value, out: &mut BTreeSet<String>) {
    for field in ["bundledDependencies", "bundleDependencies"] {
        if let Some(arr) = v.get(field).and_then(Value::as_array) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    out.insert(s.to_string());
                }
            }
        }
    }
}

/// Gather the published entry files from `main`, `module`, `bin`, and `exports`,
/// each tagged Main or Subpath. A recognized JS file is kept directly; an
/// extensionless / directory-style target (`"./dist/index"`, `"./dist"`) is kept
/// as a candidate and resolved Node-style by the graph walk; `types`/`.d.ts` and
/// asset conditions carry no runtime imports and are filtered.
fn collect_entry_points(v: &Value) -> Vec<Entry> {
    // Dedup on (kind, path) so a file exported at both `.` and a subpath seeds
    // both surfaces into the walk.
    let mut seen: BTreeSet<(u8, String)> = BTreeSet::new();
    let mut out = Vec::new();
    let mut push =
        |p: &str, kind: EntryKind, out: &mut Vec<Entry>, seen: &mut BTreeSet<(u8, String)>| {
            let norm = normalize_rel(p);
            let key = (kind as u8, norm.clone());
            if is_entry_candidate(&norm) && seen.insert(key) {
                out.push(Entry { path: norm, kind });
            }
        };

    for field in ["main", "module"] {
        if let Some(s) = v.get(field).and_then(Value::as_str) {
            push(s, EntryKind::Main, &mut out, &mut seen);
        }
    }

    match v.get("bin") {
        Some(Value::String(s)) => push(s, EntryKind::Main, &mut out, &mut seen),
        Some(Value::Object(map)) => {
            for b in map.values() {
                if let Some(s) = b.as_str() {
                    push(s, EntryKind::Main, &mut out, &mut seen);
                }
            }
        }
        _ => {}
    }

    // The top level of `exports` decides the kind: a `.`-keyed subpath map splits
    // `.` (Main) from every `./x` (Subpath); a bare condition map or string is the
    // main entry (sugar for `.`).
    if let Some(exports) = v.get("exports") {
        match exports {
            Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => {
                for (k, child) in map {
                    let kind = if k == "." {
                        EntryKind::Main
                    } else {
                        EntryKind::Subpath
                    };
                    walk_exports(child, kind, &mut out, &mut seen, &mut push);
                }
            }
            other => walk_exports(other, EntryKind::Main, &mut out, &mut seen, &mut push),
        }
    }

    // Fallback: a package with no explicit entry resolves `./index.js` (Node's
    // default main). Give the walk a root so an entry-less legacy package is
    // still analyzed.
    if out.is_empty() {
        out.push(Entry {
            path: "index.js".to_string(),
            kind: EntryKind::Main,
        });
    }
    out
}

/// Recursively collect every relative-path leaf of an `exports` subtree, carrying
/// the surface `kind` down. Skips `types`/`typings` (they point at `.d.ts`).
fn walk_exports(
    node: &Value,
    kind: EntryKind,
    out: &mut Vec<Entry>,
    seen: &mut BTreeSet<(u8, String)>,
    push: &mut impl FnMut(&str, EntryKind, &mut Vec<Entry>, &mut BTreeSet<(u8, String)>),
) {
    match node {
        Value::String(s) => push(s, kind, out, seen),
        Value::Object(map) => {
            for (k, child) in map {
                if k == "types" || k == "typings" {
                    continue;
                }
                walk_exports(child, kind, out, seen, push);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                walk_exports(child, kind, out, seen, push);
            }
        }
        _ => {}
    }
}

/// Strip a leading `./` and collapse a leading `/`; entry paths are relative to
/// the package root.
fn normalize_rel(p: &str) -> String {
    p.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// Whether a `main`/`module`/`bin`/`exports` target should SEED the reachable
/// walk. Admits a recognized JS file (parsed directly) OR an EXTENSIONLESS /
/// directory-style target — Node resolves `"./dist/index"` → `./dist/index.js`
/// and `"./dist"` → `./dist/index.js` at runtime, and the graph walk's
/// `resolve_entry` runs that SAME ladder, so on-disk resolution is deferred to
/// it (a path that resolves to nothing is simply never analyzed — no false
/// entry). A non-JS asset/type extension (`.json`/`.node`/`.wasm`/`.css`, a
/// `.d.ts` stub) carries no analyzable imports, so it is dropped here and never
/// costs a resolver probe. The `is_js_like`-only gate this replaced dropped
/// extensionless mains outright → `files_analyzed: 0` → every phantom missed.
fn is_entry_candidate(path: &str) -> bool {
    is_js_like(path) || is_sfc_like(path) || extension(path).is_none()
}

/// A JS-like runtime file (extension we can parse for imports). Excludes `.json`,
/// `.node`, `.wasm`, `.css`, and `.d.ts` type stubs.
pub fn is_js_like(path: &str) -> bool {
    if path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts") {
        return false;
    }
    matches!(
        extension(path),
        Some("js" | "cjs" | "mjs" | "jsx" | "ts" | "tsx" | "mts" | "cts")
    )
}

/// A single-file component (Astro/Vue/Svelte) whose imports live in a
/// frontmatter / `<script>` block. Under the global virtual store a backend it
/// imports there — even for TYPES ONLY — still needs project-local
/// materialization: the package's realpath escapes into the shared store, so a
/// type-checker's upward `node_modules` walk can't reach the hoisted backend
/// otherwise (nub#450). The graph walk resolves these and `extract` reads their
/// script region.
pub fn is_sfc_like(path: &str) -> bool {
    matches!(extension(path), Some("astro" | "vue" | "svelte"))
}

fn extension(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('.').map(|(_, e)| e)
}

#[cfg(test)]
mod tests {
    use super::Manifest;

    #[test]
    fn splits_optional_peers_out_of_required_and_excludes_dev() {
        let raw = br#"{
            "name": "pkg",
            "dependencies": { "a": "1" },
            "devDependencies": { "jest": "1" },
            "peerDependencies": { "react": "*", "zod": "*" },
            "peerDependenciesMeta": { "zod": { "optional": true } }
        }"#;
        let m = Manifest::parse(raw).unwrap();
        assert!(m.deps.contains("a"));
        // devDependencies are NOT in any resolvable set → phantom-eligible.
        assert!(!m.deps.contains("jest") && !m.required_peers.contains("jest"));
        assert!(m.required_peers.contains("react"));
        assert!(m.optional_peers.contains("zod")); // optional peer moved out of required
        assert!(!m.required_peers.contains("zod"));
    }

    #[test]
    fn collects_entry_points_from_exports_and_filters_types() {
        let raw = br#"{
            "name": "pkg",
            "exports": {
                ".": { "import": "./dist/index.mjs", "require": "./dist/index.cjs", "types": "./dist/index.d.ts" },
                "./sub": "./dist/sub.js"
            }
        }"#;
        let m = Manifest::parse(raw).unwrap();
        let entry = |p: &str| m.entry_points.iter().find(|e| e.path == p);
        assert_eq!(
            entry("dist/index.mjs").unwrap().kind,
            super::EntryKind::Main
        );
        assert_eq!(
            entry("dist/index.cjs").unwrap().kind,
            super::EntryKind::Main
        );
        // `./sub` is a non-`.` export → the adapter surface.
        assert_eq!(
            entry("dist/sub.js").unwrap().kind,
            super::EntryKind::Subpath
        );
        assert!(!m.entry_points.iter().any(|e| e.path.contains(".d.ts")));
    }

    #[test]
    fn direct_sfc_export_is_collected_as_entry() {
        // A package publishing an .astro/.vue/.svelte directly (not via a JS
        // re-export) must still be scanned, or its type-only phantoms are missed
        // under GVS (nub#450, codex review P1).
        let raw = br#"{
            "name": "pkg",
            "exports": { "./Icon": "./components/Icon.astro", "./Widget": "./Widget.vue" }
        }"#;
        let m = Manifest::parse(raw).unwrap();
        assert_eq!(
            m.entry_points
                .iter()
                .find(|e| e.path == "components/Icon.astro")
                .unwrap()
                .kind,
            super::EntryKind::Subpath
        );
        assert!(m.entry_points.iter().any(|e| e.path == "Widget.vue"));
    }

    #[test]
    fn keeps_extensionless_and_directory_entries_drops_asset_mains() {
        // The recall bug: an extensionless `main` (Node resolves `./dist/index`
        // → `./dist/index.js`) must survive as a candidate for the graph walk to
        // resolve — not be dropped for lacking a literal JS extension. A
        // directory-style main is the same shape. `.json`/`.node`/`.d.ts` targets
        // carry no analyzable imports and stay filtered.
        let has = |m: &Manifest, p: &str| m.entry_points.iter().any(|e| e.path == p);

        let ext = Manifest::parse(br#"{"name":"p","main":"./dist/index"}"#).unwrap();
        assert!(
            has(&ext, "dist/index"),
            "extensionless main kept as candidate"
        );

        let dir = Manifest::parse(br#"{"name":"p","main":"./dist"}"#).unwrap();
        assert!(has(&dir, "dist"), "directory-style main kept as candidate");

        let json = Manifest::parse(br#"{"name":"p","main":"./data.json"}"#).unwrap();
        assert!(
            !has(&json, "data.json"),
            ".json main dropped (not analyzable)"
        );
        // No entry survived → the entry-less fallback (`index.js`) seeds instead.
        assert_eq!(json.entry_points.len(), 1);
        assert_eq!(json.entry_points[0].path, "index.js");

        let native = Manifest::parse(br#"{"name":"p","main":"./addon.node"}"#).unwrap();
        assert!(!has(&native, "addon.node"), ".node main dropped");

        // Extensionless conditional target inside an `exports` map is admitted too.
        let exp = Manifest::parse(
            br#"{"name":"p","exports":{".":{"import":"./dist/index","types":"./dist/index.d.ts"}}}"#,
        )
        .unwrap();
        assert!(has(&exp, "dist/index"), "extensionless exports target kept");
        assert!(!exp.entry_points.iter().any(|e| e.path.contains(".d.ts")));
    }
}
