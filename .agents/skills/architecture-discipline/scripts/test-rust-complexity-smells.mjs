#!/usr/bin/env node

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const detector = join(here, "rust-complexity-smells.mjs");
const fixtures = join(here, "fixtures");

function run(args) {
  return spawnSync(process.execPath, [detector, ...args], { encoding: "utf8" });
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const positive = run([
  "--diff-file", join(fixtures, "complex.diff"),
  "--max-prod-lines", "10",
  "--max-prod-files", "1",
  "--max-types", "2",
  "--max-role-names", "1",
  "--max-primitive-hits", "1",
  "--max-lifecycle-flags", "1",
  "--max-harness-hooks", "0",
  "--max-comment-ratio", "0.15",
  "--min-comment-lines", "5",
  "--max-thin-abstractions", "0",
]);
assert(positive.status === 0, `warnings must exit successfully:\n${positive.stderr}`);
for (const id of ["scale-lines", "scale-files", "types", "role-cluster", "primitives", "lifecycle", "harness-hooks", "prose-density", "abstractions"]) {
  assert(positive.stdout.includes(`[${id}]`), `missing ${id} warning:\n${positive.stdout}`);
}

const clean = run(["--diff-file", join(fixtures, "clean.diff")]);
assert(clean.status === 0, `clean fixture failed:\n${clean.stderr}`);
assert(clean.stdout.includes("No configured thresholds crossed"), `clean fixture emitted warnings:\n${clean.stdout}`);

const usageError = run(["--not-an-option"]);
assert(usageError.status !== 0, "usage errors must fail");

process.stdout.write("rust-complexity-smells fixtures passed\n");
