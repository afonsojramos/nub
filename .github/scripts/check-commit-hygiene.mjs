#!/usr/bin/env node
// Commit-message hygiene gate.
//
// Scans every commit message in a PR range (merge-base(base, head)..head) and
// fails if any message carries an automated co-authorship / agent-attribution
// trailer. Matching is deliberately narrow: only attribution TRAILER forms fail.
//
//   - A legitimate HUMAN co-author (`Co-authored-by: Jane <jane@example.com>`)
//     must PASS — only co-author trailers naming an automated agent fail.
//   - A topical mention of a tool in a subject or body (e.g. a commit that adds
//     a docs page about a "Claude Code skill") must PASS.
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
// Co-author trailers are anchored to line-start (a trailer, not prose). Topical
// patterns deliberately have no line anchor — they catch the brand only in the
// attribution-phrase forms ("generated with ...", a session/code URL).
const RULES = [
  {
    label: "Co-authored-by trailer attributing an automated agent",
    test: (line) =>
      /^\s*co-authored-by:\s*.*(claude|anthropic\.com|noreply@anthropic)/i.test(
        line,
      ),
  },
  {
    label: "Claude-Session trailer",
    test: (line) => /^\s*claude-session:/i.test(line),
  },
  {
    label: "claude.ai/code attribution link",
    test: (line) => /claude\.ai\/code/i.test(line),
  },
  {
    label: '"Generated with Claude/AI" attribution line',
    test: (line) => /generated with (claude|ai\b)/i.test(line),
  },
  {
    label: 'robot-emoji "Generated" attribution line',
    test: (line) => /\u{1F916}.*generated/iu.test(line),
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
    `Commit-message hygiene OK — scanned ${shas.length} commit(s), no agent-attribution trailers found.`,
  );
  process.exit(0);
}

console.error("Commit-message hygiene check FAILED.\n");
console.error(
  "These commits carry automated co-authorship / agent-attribution trailers.\n" +
    "nub commit messages must not include them (legitimate human co-authors are fine).\n",
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
