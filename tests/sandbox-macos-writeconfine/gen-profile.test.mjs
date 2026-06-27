#!/usr/bin/env node
// Unit tests for gen-profile.mjs. Run: node gen-profile.test.mjs
// Asserts the load-bearing properties: write-deny floor, canonicalization of
// write-allow paths (symlink-form input -> canonical-form rule), strict vs
// relaxed cache-allowlist gating, the tight device literal set, and SBPL escaping.

import { mkdtempSync, mkdirSync, symlinkSync, rmSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build, canonicalizeForAllow, sbplEscape, DEVICE_LITERALS } from "./gen-profile.mjs";

let failures = 0;
function ok(cond, msg) {
  if (cond) console.log(`  ok  ${msg}`);
  else {
    failures++;
    console.error(`FAIL  ${msg}`);
  }
}

// A real on-disk symlinked dir so canonicalization has something to resolve.
// `tmpdir()` is itself a /var/folders firmlink -> /private/var/folders, so
// realpath the root: the generator canonicalizes to the /private form, and the
// expectations must match that (this is the firmlink-resolution the generator
// exists to do).
const root = realpathSync(mkdtempSync(join(tmpdir(), "genprof-")));
const realPkg = join(root, "real", "pkg");
mkdirSync(realPkg, { recursive: true });
const linkDir = join(root, "link");
symlinkSync(join(root, "real"), linkDir); // link/ -> real/
const symlinkedPkg = join(linkDir, "pkg"); // resolves to real/pkg

console.log("# canonicalization");
{
  const canon = canonicalizeForAllow(symlinkedPkg);
  ok(canon === realPkg, `symlinked input resolves to real path (${canon})`);
  // non-existent tail anchored at nearest existing real ancestor
  const future = join(symlinkedPkg, "build", "Release");
  ok(
    canonicalizeForAllow(future) === join(realPkg, "build", "Release"),
    "non-existent cache tail anchored at canonical existing prefix",
  );
}

console.log("# write-deny floor + device set");
{
  const prof = build({ pkg: realPkg, write: [], tmp: [], mode: "strict", devSubpath: false });
  ok(prof.includes("(deny file-write*)"), "emits deny-write floor");
  ok(prof.indexOf("(deny file-write*)") < prof.indexOf("(allow file-write*"), "deny precedes allows");
  for (const d of ["/dev/null", "/dev/tty", "/dev/urandom"])
    ok(prof.includes(`(literal "${d}")`), `device literal present: ${d}`);
  ok(!prof.includes('(subpath "/dev")'), "tight literal device set, not /dev subpath");
}

console.log("# strict emits the CANONICAL pkg path, not the symlink form");
{
  const prof = build({ pkg: symlinkedPkg, write: [], tmp: [], mode: "strict", devSubpath: false });
  ok(prof.includes(`(allow file-write* (subpath "${realPkg}"))`), "pkg allow uses canonical path");
  ok(!prof.includes(`(subpath "${symlinkedPkg}")`), "symlink-form pkg path NOT emitted (would be inert)");
}

console.log("# strict gates out the cache allowlist; relaxed admits it");
{
  const cache = join(root, "real", "cache");
  mkdirSync(cache, { recursive: true });
  const strict = build({ pkg: realPkg, write: [cache], tmp: [], mode: "strict", devSubpath: false });
  ok(!strict.includes(`(subpath "${cache}")`), "strict mode IGNORES --write cache roots");
  const relaxed = build({ pkg: realPkg, write: [cache], tmp: [], mode: "relaxed", devSubpath: false });
  ok(relaxed.includes(`(allow file-write* (subpath "${cache}"))`), "relaxed mode grants the cache root");
}

console.log("# tmp roots always granted (both modes)");
{
  const t = join(root, "real", "scratch");
  mkdirSync(t, { recursive: true });
  const strict = build({ pkg: realPkg, write: [], tmp: [t], mode: "strict", devSubpath: false });
  ok(strict.includes(`(allow file-write* (subpath "${t}"))`), "tmp granted in strict");
}

console.log("# SBPL escaping");
{
  ok(sbplEscape('a"b\\c') === 'a\\"b\\\\c', "escapes quote and backslash");
}

console.log("# devSubpath opt-out");
{
  const prof = build({ pkg: realPkg, write: [], tmp: [], mode: "strict", devSubpath: true });
  ok(prof.includes('(allow file-write* (subpath "/dev"))'), "--dev-subpath emits /dev subpath");
}

console.log("# darwin-temp grant (Apple-toolchain confstr scratch)");
{
  const off = build({ pkg: realPkg, write: [], tmp: [], mode: "strict", devSubpath: false });
  // the per-user darwin dirs end in /T" or /C"; the pkg path (also under
  // /private/var/folders) does not — so anchor on the trailing-segment quote.
  ok(!/\/T"\)\)$/m.test(off) && !/\/C"\)\)$/m.test(off), "no darwin-temp grant by default");
  const on = build({ pkg: realPkg, write: [], tmp: [], mode: "strict", devSubpath: false, darwinTemp: true });
  // getconf returns /var/folders/<uid>/{T,C}; canonical is /private/var/folders/...
  ok(/private\/var\/folders\/[^"]+\/T/.test(on), "darwin-temp grants the per-user T dir (canonical)");
  ok(/private\/var\/folders\/[^"]+\/C/.test(on), "darwin-temp grants the per-user C dir (canonical)");
}

rmSync(root, { recursive: true, force: true });
console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
