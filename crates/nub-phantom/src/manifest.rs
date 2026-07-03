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
    /// Subpath keys of the `imports` map (bare `#...` self references).
    pub imports_keys: BTreeSet<String>,
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

        if let Some(imports) = v.get("imports").and_then(Value::as_object) {
            for k in imports.keys() {
                m.imports_keys.insert(k.clone());
            }
        }

        m.entry_points = collect_entry_points(&v);
        Some(m)
    }

    /// Is `pkg` declared anywhere in the resolvable surface (dep / optional dep /
    /// any peer / bundled)? Self-references are handled by the caller.
    pub fn declares(&self, pkg: &str) -> bool {
        self.deps.contains(pkg)
            || self.required_peers.contains(pkg)
            || self.optional_peers.contains(pkg)
            || self.bundled.contains(pkg)
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
/// each tagged Main or Subpath. Only JS-like relative paths are kept;
/// `types`/`.d.ts` and asset conditions are filtered (they carry no runtime
/// imports).
fn collect_entry_points(v: &Value) -> Vec<Entry> {
    // Dedup on (kind, path) so a file exported at both `.` and a subpath seeds
    // both surfaces into the walk.
    let mut seen: BTreeSet<(u8, String)> = BTreeSet::new();
    let mut out = Vec::new();
    let mut push =
        |p: &str, kind: EntryKind, out: &mut Vec<Entry>, seen: &mut BTreeSet<(u8, String)>| {
            let norm = normalize_rel(p);
            let key = (kind as u8, norm.clone());
            if is_js_like(&norm) && seen.insert(key) {
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
        assert!(!m.declares("jest")); // devDependencies are NOT resolvable → phantom-eligible
        assert!(m.required_peers.contains("react"));
        assert!(m.optional_peers.contains("zod"));
        assert!(!m.required_peers.contains("zod"));
        assert!(m.declares("zod")); // optional peer is still declared
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
}
