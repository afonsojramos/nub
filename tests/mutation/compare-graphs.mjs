#!/usr/bin/env node
// Semantic graph comparator for the mutation differential.
//
// Compares two normalized graphs (from extract-graph.mjs) for SEMANTIC
// equality — same direct-spec map, same resolved-version multiset — ignoring
// key ordering and lockfile formatting. Used to assert nub's mutated lockfile
// is semantically equivalent to what the real PM produces for the SAME
// mutation (catalogue §M direction (b)).
//
// Both inputs MUST be the same `format` (we compare nub-<pm> vs real-<pm>,
// never cross-PM). A format mismatch is a harness wiring bug → exit 2.
//
// Usage:  compare-graphs.mjs <a.json> <b.json> [--label-a NAME] [--label-b NAME]
// Exit:   0 = semantically equal
//         1 = divergence (a human-readable diff is printed to stdout)
//         2 = usage / format-mismatch error

import fs from "node:fs";

function die(msg) {
  process.stderr.write(`compare-graphs: ${msg}\n`);
  process.exit(2);
}

const args = process.argv.slice(2);
let a = null,
  b = null,
  labelA = "A",
  labelB = "B";
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--label-a") labelA = args[++i];
  else if (args[i] === "--label-b") labelB = args[++i];
  else if (!a) a = args[i];
  else if (!b) b = args[i];
  else die(`unexpected argument: ${args[i]}`);
}
if (!a || !b) die("usage: compare-graphs.mjs <a.json> <b.json>");

const read = (f) => {
  try {
    return JSON.parse(fs.readFileSync(f, "utf8"));
  } catch (e) {
    die(`could not read/parse ${f}: ${e.message}`);
  }
};
const ga = read(a);
const gb = read(b);

if (ga.format !== gb.format) {
  die(`format mismatch: ${labelA}=${ga.format} vs ${labelB}=${gb.format} — only same-PM graphs are comparable`);
}

const diffs = [];

// Direct deps: name -> spec must match exactly on both sides.
{
  const names = new Set([...Object.keys(ga.direct || {}), ...Object.keys(gb.direct || {})]);
  for (const n of [...names].sort()) {
    const sa = (ga.direct || {})[n];
    const sb = (gb.direct || {})[n];
    if (sa === undefined) diffs.push(`direct dep present only in ${labelB}: ${n}@${sb}`);
    else if (sb === undefined) diffs.push(`direct dep present only in ${labelA}: ${n}@${sa}`);
    else if (sa !== sb) diffs.push(`direct dep spec differs for ${n}: ${labelA}=${sa}  ${labelB}=${sb}`);
  }
}

// Resolved version multiset: every name@version count must match.
{
  const keys = new Set([...Object.keys(ga.resolved || {}), ...Object.keys(gb.resolved || {})]);
  for (const k of [...keys].sort()) {
    const ca = (ga.resolved || {})[k] || 0;
    const cb = (gb.resolved || {})[k] || 0;
    if (ca !== cb) {
      if (ca === 0) diffs.push(`resolved pkg present only in ${labelB}: ${k}`);
      else if (cb === 0) diffs.push(`resolved pkg present only in ${labelA}: ${k}`);
      else diffs.push(`resolved pkg count differs for ${k}: ${labelA}=${ca}  ${labelB}=${cb}`);
    }
  }
}

if (diffs.length === 0) {
  process.stdout.write(`OK: semantically equal (${labelA} ≡ ${labelB}, format=${ga.format})\n`);
  process.exit(0);
}

process.stdout.write(`DIVERGENCE (${labelA} vs ${labelB}, format=${ga.format}):\n`);
for (const d of diffs) process.stdout.write(`  - ${d}\n`);
process.exit(1);
