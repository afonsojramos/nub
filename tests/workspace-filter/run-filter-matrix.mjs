#!/usr/bin/env node
// Differential --filter / -r matrix: for each (topology x selector) run REAL
// pnpm and nub on the SAME fixture, parse the selected package set from each,
// and assert the two sets are equal. A divergence is a recorded FAILURE that
// prints the topology, selector, and both sets so it is debuggable from CI logs
// alone -- no rerun needed.
//
// The membership oracle is `<tool> <selector> -r run whoami`: every member
// carries an identical `whoami` script printing `NUBPKG:<name>`, so the set of
// names that print is exactly the selected set. Both pnpm and nub emit those
// lines, so the parse is symmetric. (`-r` is included on BOTH invocations so the
// run engages the workspace member set; pnpm requires it, and it is a
// no-op-safe addition for nub.)
//
// Usage:
//   node tests/workspace-filter/run-filter-matrix.mjs [nub-binary] [fixtures-dir]
// Defaults: nub = target/release/nub (then target/debug/nub); fixtures =
// /tmp/nub-wsfilter-fixtures (regenerated each run via make-fixtures.mjs).
//
// Skips cleanly (exit 0, message) when pnpm is not on PATH -- CI without pnpm
// must not fail. Exits non-zero iff a NEW divergence is found (known, triaged
// divergences are XFAIL and do not fail the run; see KNOWN_DIVERGENCES below).

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..", "..");

// --- resolve tools ---------------------------------------------------------
function which(cmd) {
  const r = spawnSync("sh", ["-c", `command -v ${cmd}`], { encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : null;
}

const PNPM = which("pnpm");
if (!PNPM) {
  console.error(
    "SKIP: pnpm not found on PATH -- the differential suite needs real pnpm as the ground-truth oracle. Install pnpm to run it.",
  );
  process.exit(0);
}

let NUB = process.argv[2];
if (!NUB) {
  NUB = join(REPO, "target/release/nub");
  if (!existsSync(NUB)) NUB = join(REPO, "target/debug/nub");
}
NUB = resolve(NUB);
if (!existsSync(NUB)) {
  console.error(`error: nub binary not found at ${NUB} (build it first)`);
  process.exit(2);
}

const FIXTURES = resolve(process.argv[3] || "/tmp/nub-wsfilter-fixtures");

// --- (re)generate fixtures -------------------------------------------------
execFileSync(process.execPath, [join(HERE, "make-fixtures.mjs"), FIXTURES], {
  stdio: ["ignore", "ignore", "inherit"],
});
const topologies = JSON.parse(
  readFileSync(join(FIXTURES, "topologies.json"), "utf8"),
);

// --- the selector matrix ---------------------------------------------------
// Per topology: the selectors that exercise a DISTINCT code path on THIS shape.
// Each entry is { args: [...selector flags], desc }. `-r` is appended to both
// invocations by the runner, so a bare `-r` case is just `{ args: [] }`.
//
// Comprehensive-not-exhaustive: each selector grammar form appears where the
// topology actually distinguishes it (e.g. `...pkg` only means something where
// pkg HAS dependents). We do not re-run an identical assertion across
// topologies that cannot tell two selectors apart.
function matrixFor(topo) {
  const F = (s) => ["--filter", s];
  const cases = [];
  const add = (args, desc) => cases.push({ args, desc });

  switch (topo.name) {
    case "linear": // a -> b -> c -> d
      add([], "-r alone selects all four");
      add(F("b"), "exact name");
      add(F("b..."), "pkg... : b + dependencies (c,d)");
      add(F("...b"), "...pkg : b + dependents (a)");
      add(F("...b..."), "...pkg... : both directions");
      add(F("b^..."), "pkg^... : dependencies only, exclude self");
      add(F("...^b"), "...^pkg : dependents only, exclude self");
      add(F("!b"), "!pkg : exclude one, complement");
      add([...F("a"), ...F("c")], "repeated --filter : union");
      add([...F("a..."), ...F("!c")], "include-expansion minus exclude");
      break;

    case "diamond": // a -> {b,c} -> d
      add(F("a..."), "a... : whole graph via deps (a,b,c,d)");
      add(F("...d"), "...d : every dependent of the shared leaf (a,b,c)");
      add(F("...d..."), "...d... : d + all dependents (whole graph)");
      add(F("d^..."), "d^... : dependencies of leaf only (empty set)");
      add(F("...^d"), "...^d : dependents of d, excluding d (a,b,c)");
      add([...F("b"), ...F("c")], "two exact filters union (b,c)");
      break;

    case "wide-fan": // hub -> leaf1..6
      add(F("hub..."), "hub... : hub + all six leaves");
      add(F("...leaf3"), "...leaf3 : leaf3 + its sole dependent hub");
      add(F("leaf*"), "name glob leaf* : all six leaves, not hub");
      add([...F("!hub")], "!hub : every leaf, hub removed");
      add(
        [...F("hub..."), ...F("!leaf1"), ...F("!leaf2")],
        "expand then subtract two",
      );
      break;

    case "islands": // x1 -> x2 | y1 -> y2
      add(F("x1..."), "x1... : only island X (x1,x2), island Y untouched");
      add(F("...x2"), "...x2 : x2 + its dependent x1, never the Y island");
      add([...F("x1..."), ...F("y1...")], "union of both islands' deps");
      add(F("!x2"), "!x2 : everything except x2 (x1,y1,y2)");
      break;

    case "nested-dirs": // packages/{core,util}, apps/{web,api}
      add(["--filter", "./apps"], "dir parent selector ./apps");
      add(["--filter", "./apps/*"], "dir glob ./apps/* : web,api");
      add(["--filter", "{packages}"], "{dir} selector");
      add(["--filter", "./packages/core"], "exact dir : core only");
      add(F("...core"), "...core : core + every dependent (util,web,api)");
      add([...F("./apps"), ...F("!api")], "dir parent minus one");
      break;

    case "dir-is-package-and-parent": // packages/group is a package AND parent of packages/group/child
      add(
        ["--filter", "./packages/group"],
        "bare dir that is a package AND a parent : selects only group, not the nested child",
      );
      add(
        ["--filter", "./packages/group/*"],
        "explicit glob reaches the nested child only",
      );
      add(["--filter", "{packages/group}"], "{dir} bare form : group only");
      break;

    case "dev-prod-mix": // app -(prod)-> lib, app -(dev)-> tool
      add(F("app..."), "app... : app + prod(lib) + dev(tool) deps");
      add(F("...tool"), "...tool : tool + its (dev-)dependent app");
      add(F("...lib"), "...lib : lib + its (prod-)dependent app");
      break;
  }
  return cases;
}

// --- known, reported divergences -------------------------------------------
// A divergence we have already triaged and filed lives here so the suite stays
// GREEN on it (it is tracked, not a regression) while STILL failing loudly on
// any NEW, unexpected divergence. Keyed by `<topology> <selectorLabel>`.
// Remove an entry once nub's source is fixed so the matrix re-asserts parity.
//
// (D1 -- bare directory selector recursion -- was FIXED 2026-06-15: a bare dir
// now matches only the exact package dir, matching pnpm's default glob
// dir-filtering. The matrix re-asserts parity on `./apps`, `{packages}`, and the
// `dir-is-package-and-parent` topology.)
const KNOWN_DIVERGENCES = new Set([]);

// --- run a tool, parse the selected set ------------------------------------
const NAME_RE = /NUBPKG:([A-Za-z0-9@/_.-]+)/g;

function selectedSet(toolPath, cwd, selectorArgs) {
  // `<tool> <selector...> -r run whoami`. We always pass -r so the workspace
  // member set engages. Capture stdout; parse NUBPKG:<name> sentinels.
  const r = spawnSync(toolPath, [...selectorArgs, "-r", "run", "whoami"], {
    cwd,
    encoding: "utf8",
  });
  const set = new Set();
  let m;
  const out = r.stdout || "";
  while ((m = NAME_RE.exec(out)) !== null) set.add(m[1]);
  NAME_RE.lastIndex = 0;
  return { set, status: r.status, stderr: r.stderr || "", stdout: out };
}

// pnpm echoes the script body (which literally contains `NUBPKG:`) in some
// reporters. The regex requires >=1 name char and the body's `NUBPKG:` is
// followed by `'+require`, where `'` is not in the class, so it yields nothing.
// Verified empirically; no extra filtering needed.

function eqSet(a, b) {
  if (a.size !== b.size) return false;
  for (const x of a) if (!b.has(x)) return false;
  return true;
}
const fmt = (s) => "{" + [...s].sort().join(", ") + "}";

// --- drive the matrix ------------------------------------------------------
let pass = 0;
let fail = 0;
let known = 0;
const failures = [];

console.error(`nub:  ${NUB}`);
console.error(`pnpm: ${PNPM}`);
console.error(`fixtures: ${FIXTURES}\n`);

for (const topo of topologies) {
  const cwd = join(FIXTURES, topo.name);
  const cases = matrixFor(topo);
  console.error(`# ${topo.name} -- ${topo.doc}`);
  for (const c of cases) {
    const selLabel = c.args.length ? c.args.join(" ") : "(-r only)";
    const want = selectedSet(PNPM, cwd, c.args);
    const got = selectedSet(NUB, cwd, c.args);

    // A pnpm run that errors out (bad selector for the version) is a harness
    // problem, not a divergence -- surface it distinctly. (pnpm's "no projects
    // matched" still exits 0, so this only catches genuine errors.)
    if (want.status !== 0 && want.set.size === 0) {
      console.error(
        `  ??   ${selLabel}\n       pnpm exited ${want.status} with empty set -- skipping (${want.stderr.trim().split("\n")[0]})`,
      );
      continue;
    }

    const key = `${topo.name} ${selLabel}`;
    if (eqSet(want.set, got.set)) {
      pass++;
      console.error(`  ok   ${selLabel}  -> ${fmt(got.set)}`);
      if (KNOWN_DIVERGENCES.has(key)) {
        console.error(
          `       !! ${key} is in KNOWN_DIVERGENCES but now MATCHES -- remove the entry (nub may have been fixed).`,
        );
      }
    } else if (KNOWN_DIVERGENCES.has(key)) {
      known++;
      console.error(
        `  XFAIL ${selLabel}  (known divergence, see README)\n        pnpm: ${fmt(want.set)}\n        nub:  ${fmt(got.set)}`,
      );
    } else {
      fail++;
      const detail = {
        topology: topo.name,
        selector: selLabel,
        desc: c.desc,
        pnpm: fmt(want.set),
        nub: fmt(got.set),
        nubStderr: got.stderr.trim().split("\n").slice(0, 3).join(" | "),
      };
      failures.push(detail);
      console.error(
        `  FAIL ${selLabel}  (${c.desc})\n       pnpm: ${detail.pnpm}\n       nub:  ${detail.nub}` +
          (got.status !== 0
            ? `\n       nub exited ${got.status}: ${detail.nubStderr}`
            : ""),
      );
    }
  }
  console.error("");
}

console.error(
  `${pass} passed, ${fail} failed, ${known} known-divergence (XFAIL)`,
);
if (fail) {
  console.error("\n=== NEW DIVERGENCES (pnpm vs nub) ===");
  for (const f of failures) {
    console.error(
      `[${f.topology}] ${f.selector}\n  desc: ${f.desc}\n  pnpm: ${f.pnpm}\n  nub:  ${f.nub}`,
    );
  }
  process.exit(1);
}
if (known) {
  console.error(
    `\nNo NEW divergences. ${known} known divergence(s) remain (XFAIL) -- see tests/workspace-filter/README.md, "Known divergences".`,
  );
} else {
  console.error("All selector sets match pnpm.");
}
