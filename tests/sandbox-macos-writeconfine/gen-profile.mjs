#!/usr/bin/env node
// SBPL write-confine profile generator for the macOS build-jail.
//
// Emits a Seatbelt (SBPL) profile that confines a lifecycle script's filesystem
// WRITES to an explicit allow-set while leaving reads/exec broad (so the
// toolchain — cc/node/python/make — works). This is the macOS write-confine
// mechanism nub's production backend (`crates/nub-sandbox/src/backend/macos.rs`)
// should generate; this generator is the validated reference + the harness's
// profile source.
//
// THE LOAD-BEARING CORRECTNESS RULE (proven empirically — see results.md §Bypass):
// Seatbelt evaluates every rule against the CANONICAL path (it resolves symlinks,
// `..`, and the macOS firmlinks `/tmp`->`/private/tmp`, `/var`->`/private/var`).
// So a write-allow given in SYMLINK form (`/tmp/x`, `/var/folders/.../pkg`) is
// INERT — it never matches the canonical path the kernel checks, and the write is
// silently DENIED (fail-closed breakage: every build under a `/tmp`- or
// `/var`-rooted project dir would lose all writes). Therefore every write-allow
// path MUST be canonicalized to its real form before it is emitted. This
// generator canonicalizes each allow root (resolving the nearest existing
// ancestor for not-yet-created cache dirs).
//
// Usage:
//   gen-profile.mjs --pkg <dir> [--project <dir>] [--write <dir>]... \
//                   [--tmp <dir>] [--mode strict|relaxed] [--dev-subpath]
//
//   --pkg <dir>       the package dir whose lifecycle script runs (read+write). Required.
//   --project <dir>   project root (read-only; documented, not write-granted).
//   --write <dir>     an extra writable root (repeatable) — the cache allowlist.
//                     In strict mode these are IGNORED (pkg + tmp only).
//   --tmp <dir>       a private scratch dir to grant write (repeatable).
//   --mode strict     pkg + sandbox-home/tmp only, NO cache allowlist (default).
//   --mode relaxed    pkg + tmp + every --write root (the cache allowlist).
//   --dev-subpath     grant write to all of /dev (subpath) instead of the tight
//                     literal device set. Default is the tight literal set.
//   --darwin-temp     also grant the per-user DARWIN_USER_TEMP_DIR +
//                     DARWIN_USER_CACHE_DIR (`/private/var/folders/<uid>/{T,C}`).
//                     The Apple toolchain (xcrun/cc/libtool) writes its `xcrun_db`
//                     scratch there via confstr — NOT redirectable by TMPDIR — so a
//                     from-source compile spews EPERM noise without it. Per-user OS
//                     scratch, low risk. Granted in BOTH modes when set.
//
// Output: the SBPL profile text on stdout.

import { realpathSync, existsSync } from "node:fs";
import { resolve, dirname, sep } from "node:path";
import { execFileSync } from "node:child_process";

// The minimal char-device write set a Node/native build actually touches.
// Proven sufficient empirically (results.md §Device): /dev/null is required
// (build tooling redirects to it); the rest are the standard char devices
// node/cc/make open for writing. Tighter than a blanket `(subpath "/dev")`.
const DEVICE_LITERALS = [
  "/dev/null",
  "/dev/zero",
  "/dev/tty",
  "/dev/dtracehelper",
  "/dev/random",
  "/dev/urandom",
  "/dev/stdout",
  "/dev/stderr",
  "/dev/fd",
];

function parseArgs(argv) {
  const out = { pkg: null, project: null, write: [], tmp: [], mode: "strict", devSubpath: false, darwinTemp: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--pkg") out.pkg = argv[++i];
    else if (a === "--project") out.project = argv[++i];
    else if (a === "--write") out.write.push(argv[++i]);
    else if (a === "--tmp") out.tmp.push(argv[++i]);
    else if (a === "--mode") out.mode = argv[++i];
    else if (a === "--dev-subpath") out.devSubpath = true;
    else if (a === "--darwin-temp") out.darwinTemp = true;
    else throw new Error(`unknown arg: ${a}`);
  }
  if (!out.pkg) throw new Error("--pkg is required");
  if (out.mode !== "strict" && out.mode !== "relaxed")
    throw new Error(`--mode must be strict|relaxed, got ${out.mode}`);
  return out;
}

// Canonicalize a path for a write-allow rule. Resolves symlinks/firmlinks/`..`
// to the form the Seatbelt kernel check sees. For a path that does not exist yet
// (a cache dir created later by the build), realpath the nearest EXISTING
// ancestor and re-append the non-existing tail — so the emitted subpath is still
// anchored at the real on-disk prefix.
function canonicalizeForAllow(p) {
  let abs = resolve(p);
  if (existsSync(abs)) return realpathSync(abs);
  // walk up to nearest existing ancestor
  let tail = [];
  let cur = abs;
  while (cur !== dirname(cur) && !existsSync(cur)) {
    tail.unshift(cur.slice(dirname(cur).length + 1));
    cur = dirname(cur);
  }
  const realPrefix = existsSync(cur) ? realpathSync(cur) : cur;
  return tail.length ? realPrefix + sep + tail.join(sep) : realPrefix;
}

function sbplEscape(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function subpathRule(p) {
  return `(allow file-write* (subpath "${sbplEscape(p)}"))`;
}

// The per-user Apple-toolchain scratch dirs (`/var/folders/<uid>/{T,C}`), read by
// `xcrun`/cc/libtool via confstr — NOT redirectable via TMPDIR. Queried from
// `getconf` so the <uid> hash is correct on any machine. Returns canonical paths.
function darwinTempDirs() {
  const dirs = [];
  for (const key of ["DARWIN_USER_TEMP_DIR", "DARWIN_USER_CACHE_DIR"]) {
    try {
      const d = execFileSync("getconf", [key], { encoding: "utf8" }).trim();
      if (d) dirs.push(canonicalizeForAllow(d));
    } catch {
      /* non-macOS or getconf missing — skip */
    }
  }
  return dirs;
}

function build(opts) {
  const rules = ["(version 1)", "(allow default)", "(deny file-write*)"];

  // Device writes (tight literal set by default).
  if (opts.devSubpath) {
    rules.push(subpathRule("/dev"));
  } else {
    const lits = DEVICE_LITERALS.map((d) => `(literal "${d}")`).join(" ");
    rules.push(`(allow file-write* ${lits})`);
  }

  // Collect the write roots for the chosen mode, canonicalize, dedup.
  const roots = [];
  const add = (p) => {
    if (!p) return;
    const c = canonicalizeForAllow(p);
    if (!roots.includes(c)) roots.push(c);
  };

  add(opts.pkg); // pkg dir: always writable
  for (const t of opts.tmp) add(t); // private scratch
  if (opts.darwinTemp) for (const d of darwinTempDirs()) add(d); // Apple-toolchain confstr scratch
  if (opts.mode === "relaxed") {
    for (const w of opts.write) add(w); // the cache allowlist
  }

  for (const r of roots) rules.push(subpathRule(r));

  return rules.join("\n") + "\n";
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  process.stdout.write(build(opts));
}

// Exported for unit tests; run as CLI when invoked directly.
export { canonicalizeForAllow, build, sbplEscape, parseArgs, DEVICE_LITERALS };

if (import.meta.url === `file://${process.argv[1]}`) main();
