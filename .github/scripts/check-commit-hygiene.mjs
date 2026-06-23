#!/usr/bin/env node
// Commit-message hygiene gate.
//
// Scans every commit message in a PR range (merge-base(base, head)..head) and
// fails if any message carries a Co-authored-by trailer naming an automated
// agent (Claude, Anthropic, noreply@anthropic). That trailer renders as a
// co-author avatar in the GitHub UI — it is the one UI-visible attribution form
// that requires enforcement.
//
//   - A legitimate HUMAN co-author (`Co-authored-by: Jane <jane@example.com>`)
//     must PASS — only trailers naming an automated agent fail.
//   - Body-text mentions of tools (e.g. Claude-Session URLs, "Generated with"
//     lines) are commit prose; they are invisible in the GitHub UI and are NOT
//     flagged by this check.
//
// No dependencies; runs on the Actions-provided Node.

import { execFileSync } from "node:child_process";

const [, , baseSha, headSha] = process.argv;
if (!baseSha || !headSha) {
  console.error("usage: check-commit-hygiene.mjs <base-sha> <head-sha>");
  process.exit(2);
}

function git(args) {
  return execFileSync("git", args, { encoding: "utf8" });
}

// Patterns. Each entry: a human-readable label + a test() over a single line.
//
// Scope: only UI-visible attribution forms. Co-authored-by trailers name a
// co-author that renders as an avatar in the GitHub UI — that is the one form
// that matters. Claude-Session URLs and "Generated with" body text are commit-
// body prose; they are invisible in the GitHub PR/commit UI and are not flagged.
// Legitimate human Co-authored-by entries pass through unchanged.
const RULES = [
  {
    label: "Co-authored-by trailer attributing an automated agent",
    test: (line) =>
      /^\s*co-authored-by:\s*.*(claude|anthropic\.com|noreply@anthropic)/i.test(
        line,
      ),
  },
];

// Resolve the range. Prefer the true merge base; fall back to base..head if the
// merge base can't be computed (shallow edge cases).
let range;
try {
  const mergeBase = git(["merge-base", baseSha, headSha]).trim();
  range = `${mergeBase}..${headSha}`;
} catch {
  range = `${baseSha}..${headSha}`;
}

const shas = git(["rev-list", range]).trim().split("\n").filter(Boolean);

const offenders = [];
for (const sha of shas) {
  const message = git(["log", "-1", "--format=%B", sha]);
  const hits = [];
  for (const line of message.split("\n")) {
    for (const rule of RULES) {
      if (rule.test(line)) hits.push({ rule: rule.label, line: line.trim() });
    }
  }
  if (hits.length) {
    const subject = git(["log", "-1", "--format=%s", sha]).trim();
    offenders.push({ sha, subject, hits });
  }
}

if (offenders.length === 0) {
  console.log(
    `Commit-message hygiene OK — scanned ${shas.length} commit(s), no agent co-authored-by trailers found.`,
  );
  process.exit(0);
}

console.error("Commit-message hygiene check FAILED.\n");
console.error(
  "These commits carry a Co-authored-by trailer naming an automated agent.\n" +
    "Remove the trailer line (legitimate human co-authors are fine).\n",
);
for (const { sha, subject, hits } of offenders) {
  console.error(`  commit ${sha.slice(0, 12)}  ${subject}`);
  for (const { rule, line } of hits) {
    console.error(`      - ${rule}: "${line}"`);
  }
  console.error("");
}
console.error(
  "To fix: amend or interactively rebase to remove the offending trailer line(s), then force-push.\n" +
    "  - Latest commit:   git commit --amend       (delete the trailer line in the editor)\n" +
    "  - Older commits:   git rebase -i <base>      (mark them 'reword', delete the trailer line)\n" +
    "  - Then:            git push --force-with-lease",
);
process.exit(1);
