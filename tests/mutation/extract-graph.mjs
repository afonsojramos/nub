#!/usr/bin/env node
// Semantic dependency-graph extractor for the mutation differential.
//
// Given a project directory, find its lockfile (pnpm / npm / bun) and emit a
// NORMALIZED, order-insensitive view of the resolved graph:
//
//   {
//     "format": "pnpm" | "npm" | "bun",
//     "direct":   { "<name>": "<declared-spec>", ... },   // root importer deps
//     "resolved": { "<name>@<version>": <count>, ... }     // every resolved pkg
//   }
//
// WHY this shape is the semantic signal (and byte-`cmp` is not):
//   - `resolved` is the MULTISET of every concrete package@version in the
//     lockfile. It captures the three things a mutation changes and the three
//     bug classes the suite hunts:
//       * add  (M.1)  — the new dep + its transitives APPEAR in the set.
//       * dedup (M.3) — whether a shared transitive collapses to one version
//                       or keeps two shows up as one-vs-two keys in the set.
//       * prune (M.5/M.6) — removed/kept transitives are present/absent.
//     It is independent of each PM's nesting LAYOUT (npm path nesting vs pnpm
//     flat `name@ver` keys vs bun `parent/child` path keys), which legitimately
//     differs and which byte-identity would false-fail on.
//   - `direct` is the root importer's declared specifiers (name -> range). It
//     captures the manifest-side mutation: `add pkg@^1` must write `^1`
//     verbatim, `remove` must drop the entry. This is the declared-spec-
//     preservation axis (catalogue 1.1/1.2/M.2).
//
// The comparator (compare-graphs.mjs) diffs two of these JSON blobs for
// equality, ignoring key ordering. Same PM on both sides — we compare
// nub's-mutated-<pm>-lockfile vs real-<pm>'s-mutated-lockfile, never cross-PM
// (PMs legitimately resolve differently from each other; each must match ITS
// OWN reference).
//
// Usage:  extract-graph.mjs <project-dir> [--format pnpm|npm|bun]
//         (auto-detects the lockfile when --format is omitted)

import fs from "node:fs";
import path from "node:path";

function die(msg) {
  process.stderr.write(`extract-graph: ${msg}\n`);
  process.exit(2);
}

const args = process.argv.slice(2);
let dir = null;
let forced = null;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--format") forced = args[++i];
  else if (!dir) dir = args[i];
  else die(`unexpected argument: ${args[i]}`);
}
if (!dir) die("usage: extract-graph.mjs <project-dir> [--format pnpm|npm|bun]");

const has = (f) => fs.existsSync(path.join(dir, f));
let format = forced;
if (!format) {
  if (has("pnpm-lock.yaml")) format = "pnpm";
  else if (has("package-lock.json")) format = "npm";
  else if (has("bun.lock")) format = "bun";
  else die(`no lockfile (pnpm-lock.yaml / package-lock.json / bun.lock) in ${dir}`);
}

const bump = (obj, key) => {
  obj[key] = (obj[key] || 0) + 1;
};

// ── pnpm: pnpm-lock.yaml ──────────────────────────────────────────────────
// `importers.<.>.{dependencies,devDependencies,optionalDependencies}` carries
// the root direct specs (each entry: `name: { specifier, version }`). The
// flat `packages:` section keys are `name@version` (scoped: `@scope/name@ver`,
// peer-suffixed: `name@ver(peerdep@x)` — we strip the peer suffix and keep the
// base name@version). A tiny hand YAML walk avoids a yaml dep (the structure we
// need is shallow + regular).
function extractPnpm(text) {
  const direct = {};
  const resolved = {};
  const lines = text.split("\n");

  // Walk the `importers:` block, root importer `.` only (the `.:` two-space key).
  // Direct deps live under `dependencies:` / `devDependencies:` /
  // `optionalDependencies:` as `name:` then `specifier: <spec>` / `version:`.
  let i = 0;
  for (; i < lines.length; i++) if (/^importers:\s*$/.test(lines[i])) break;
  if (i < lines.length) {
    i++;
    // find the root `  .:` importer
    for (; i < lines.length; i++) {
      if (/^\S/.test(lines[i])) break; // left the importers block
      if (/^ {2}(['"]?)\.\1:\s*$/.test(lines[i])) {
        i++;
        // inside root importer: 4-space dep-bucket headers, 6-space names
        let bucket = null;
        for (; i < lines.length; i++) {
          const l = lines[i];
          if (/^ {0,3}\S/.test(l) || /^ {2}\S/.test(l)) {
            i--;
            break;
          } // dedent out of importer
          let m;
          if ((m = l.match(/^ {4}(dependencies|devDependencies|optionalDependencies):\s*$/))) {
            bucket = m[1];
          } else if (bucket && (m = l.match(/^ {6}(\S+?):\s*$/))) {
            const name = m[1].replace(/^['"]|['"]$/g, "");
            // next line(s): specifier: <spec>
            let spec = "*";
            for (let j = i + 1; j < lines.length && /^ {8}/.test(lines[j]); j++) {
              const sm = lines[j].match(/^ {8}specifier:\s*(.+?)\s*$/);
              if (sm) {
                spec = sm[1].replace(/^['"]|['"]$/g, "");
                break;
              }
            }
            direct[name] = spec;
          }
        }
        break;
      }
    }
  }

  // `packages:` keys -> name@version multiset.
  let inPkgs = false;
  for (const l of lines) {
    if (/^packages:\s*$/.test(l)) {
      inPkgs = true;
      continue;
    }
    if (inPkgs) {
      if (/^\S/.test(l)) break; // next top-level section (snapshots:)
      const m = l.match(/^ {2}(\S.*?):\s*$/);
      if (m) {
        let key = m[1].replace(/^['"]|['"]$/g, "");
        key = key.replace(/\([^)]*\)/g, ""); // strip peer-dep suffix
        resolved[key] = (resolved[key] || 0) + 1;
      }
    }
  }
  return { format: "pnpm", direct, resolved };
}

// ── npm: package-lock.json (lockfileVersion 3) ────────────────────────────
// `packages[""]` is the root: its dependencies/devDependencies/optional carry
// the direct specs. Every other `packages["node_modules/.../<name>"]` entry is
// a resolved package; the LAST path segment after `node_modules/` is the name,
// and `.version` is the version. Nested duplicates (`.../node_modules/x`) yield
// the same name at possibly-different versions — exactly the multiset we want.
function extractNpm(text) {
  const lock = JSON.parse(text);
  const direct = {};
  const resolved = {};
  const root = (lock.packages && lock.packages[""]) || {};
  for (const bucket of ["dependencies", "devDependencies", "optionalDependencies"]) {
    for (const [name, spec] of Object.entries(root[bucket] || {})) direct[name] = spec;
  }
  for (const [key, entry] of Object.entries(lock.packages || {})) {
    if (key === "") continue;
    if (!entry || entry.link) continue; // workspace symlink entry, not a real pkg
    const segs = key.split("node_modules/");
    const name = segs[segs.length - 1].replace(/\/$/, "");
    const version = entry.version;
    if (!version) continue;
    bump(resolved, `${name}@${version}`);
  }
  return { format: "npm", direct, resolved };
}

// ── bun: bun.lock (JSONC — trailing commas) ───────────────────────────────
// `workspaces[""].{dependencies,devDependencies,optionalDependencies}` carries
// the root direct specs. `packages` is a map whose VALUES are arrays whose
// FIRST element is `"<name>@<version>"`. The map KEY is a nesting path
// (`parent/child`) — we ignore it and read name@version off the value tuple,
// which is the resolved package.
function extractBun(text) {
  // bun.lock is JSON with trailing commas; strip them for JSON.parse.
  const cleaned = text.replace(/,(\s*[}\]])/g, "$1");
  const lock = JSON.parse(cleaned);
  const direct = {};
  const resolved = {};
  const root = (lock.workspaces && lock.workspaces[""]) || {};
  for (const bucket of ["dependencies", "devDependencies", "optionalDependencies"]) {
    for (const [name, spec] of Object.entries(root[bucket] || {})) direct[name] = spec;
  }
  for (const tuple of Object.values(lock.packages || {})) {
    const id = Array.isArray(tuple) ? tuple[0] : null;
    if (typeof id !== "string") continue;
    // id is `name@version` (scoped: `@scope/name@version`). Split on LAST `@`.
    const at = id.lastIndexOf("@");
    if (at <= 0) continue;
    const name = id.slice(0, at);
    const version = id.slice(at + 1);
    // A workspace member entry has an empty/path version — skip non-semver-ish.
    if (!version || version.startsWith("workspace:")) continue;
    bump(resolved, `${name}@${version}`);
  }
  return { format: "bun", direct, resolved };
}

const lockPath = {
  pnpm: "pnpm-lock.yaml",
  npm: "package-lock.json",
  bun: "bun.lock",
}[format];
if (!lockPath) die(`unknown format: ${format}`);
const full = path.join(dir, lockPath);
if (!fs.existsSync(full)) die(`expected ${lockPath} in ${dir}`);
const text = fs.readFileSync(full, "utf8");

let out;
if (format === "pnpm") out = extractPnpm(text);
else if (format === "npm") out = extractNpm(text);
else out = extractBun(text);

process.stdout.write(JSON.stringify(out, null, 2) + "\n");
