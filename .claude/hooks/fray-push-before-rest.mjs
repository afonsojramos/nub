#!/usr/bin/env node
// @ts-check
/**
 * fray — SubagentStop hook. The HARD GUARANTEE behind the push-then-EXIT rule:
 * refuse to let a sub-agent come to rest while a feature/PR worktree holds a
 * committed-but-UNPUSHED change (local branch tip ahead of its remote upstream).
 *
 * WHY: the #1 strand failure is an agent that creates a commit/merge in a worktree,
 * then rests (e.g. "I'll wait for CI") BEFORE pushing — leaving the only copy of
 * verified work local until the orchestrator manually re-pokes it (~20min strands
 * observed). This hook converts the *rule* "push the instant a commit exists" into a
 * structural floor: an agent literally cannot rest on unpushed work.
 *
 * HOW (scoped to the agent's OWN worktree — revised 2026-06-21):
 *   - BLOCK the rest ONLY when the resting agent's OWN `cwd` (from the SubagentStop payload)
 *     is a worktree whose non-main branch is ahead of its upstream. This is the hard
 *     guarantee, and it can never hand agent A a push it can't do — A's own cwd is A's tree.
 *   - When cwd is NOT a stranded worktree (the common case — agents often `cd` into their
 *     worktree per-command rather than as a persistent cwd, so cwd is usually the shared
 *     tree), we CANNOT attribute a sibling worktree's strand to this agent, so we DO NOT
 *     block — instead we WARN (non-blocking `systemMessage`) if any `/tmp/nub-wt-*` worktree
 *     is unpushed, and ALLOW the rest. Warn-not-block here keeps fail-open totality.
 *
 * NARROW (never over-blocks):
 *   - block only on the agent's OWN cwd worktree (never a sibling/orphan);
 *   - never the shared main tree, never a branch named `main`/`master`/detached `HEAD`;
 *   - only a branch WITH an upstream that is provably ahead (rev-list count > 0) — a branch
 *     with no upstream (never pushed) is NOT flagged (can't distinguish "about to push" from
 *     "scratch branch"; a false block there would be the wedge we must avoid);
 *   - uncommitted-only / clean state is never flagged (only a real local-ahead commit).
 *
 * FAIL-OPEN, ABSOLUTELY: any error, any uncertainty → allow the rest (emit {} / exit 0).
 * A bug here could wedge EVERY agent, so every path degrades to "allow". We never throw,
 * never block on a git command that errored, and never block on parse failure. The ONLY
 * blocking path is "the agent's own cwd worktree is provably ahead of its remote".
 */
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

/** Allow the rest (the safe default). @returns {never} */
function allow() {
  process.stdout.write('{}');
  process.exit(0);
}

/**
 * Block the rest and feed a reason back to the sub-agent.
 * @param {string} reason
 * @returns {never}
 */
function block(reason) {
  process.stdout.write(JSON.stringify({ decision: 'block', reason }));
  process.exit(0);
}

/**
 * Run a git command in a given dir; return trimmed stdout, or null on ANY error.
 * @param {string} cwd
 * @param {string[]} args
 * @returns {string|null}
 */
function git(cwd, args) {
  try {
    return execFileSync('git', ['-C', cwd, ...args], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 5000,
    }).trim();
  } catch {
    return null;
  }
}

try {
  // Parse stdin (SubagentStop payload). A parse failure must not block.
  let payload = {};
  try {
    const raw = readFileSync(0, 'utf8');
    if (raw.trim()) payload = JSON.parse(raw);
  } catch {
    allow();
  }

  // Guard against an infinite block loop: if a prior fray block is already active for
  // this stop, do not stack another. (SubagentStop may re-fire; never wedge.)
  if (payload && payload.stop_hook_active === true) allow();

  const projectDir = process.env.CLAUDE_PROJECT_DIR || process.cwd();

  /**
   * Is `dir` a git worktree whose branch (non-main, with an upstream) is ahead of its
   * remote? Returns {branch, ahead} when stranded, else null. Fail-open (null) on any error.
   * @param {string} dir
   * @returns {{branch:string, ahead:number}|null}
   */
  function strandedAt(dir) {
    if (!dir) return null;
    const branch = git(dir, ['rev-parse', '--abbrev-ref', 'HEAD']);
    if (!branch || branch === 'HEAD' || branch === 'main' || branch === 'master') return null;
    const upstream = git(dir, ['rev-parse', '--abbrev-ref', '--symbolic-full-name', '@{upstream}']);
    if (!upstream) return null; // no upstream → can't prove unpushed → don't block (fail-open)
    const aheadStr = git(dir, ['rev-list', '--count', '@{upstream}..HEAD']);
    if (aheadStr === null) return null; // git errored → fail-open
    const ahead = Number.parseInt(aheadStr, 10);
    return Number.isFinite(ahead) && ahead > 0 ? { branch, ahead } : null;
  }

  // SCOPE TO THE AGENT'S OWN WORKTREE. The SubagentStop payload's `cwd` is the resting
  // agent's working dir. The HARD guarantee — BLOCK the rest — applies ONLY when the agent's
  // OWN cwd is a worktree with an unpushed commit, so we never hand agent A a push it can't do
  // for a sibling/orphan worktree. (Fixed 2026-06-21: the prior all-worktrees scan could block
  // A for B's strand.)
  const cwd = typeof payload?.cwd === 'string' ? payload.cwd : '';
  const own = strandedAt(cwd);
  if (own) {
    block(
      `⟦fray push-before-rest guard⟧ You are coming to rest with a committed-but-UNPUSHED change in your worktree:\n` +
        `  • ${cwd} (branch ${own.branch}) is ${own.ahead} commit(s) ahead of its remote — UNPUSHED.\n\n` +
        `Per the push-then-EXIT rule: a commit is stranded until it's pushed. PUSH IT NOW before resting — ` +
        `\`git push origin HEAD:${own.branch}\` (safe pre-CI; CI runs on the pushed commit). ` +
        `Do NOT arm a CI watcher or poll loop afterward — push, report "pushed <sha>, awaiting CI" (and append a line to ` +
        `.fray/merge-queue.jsonl if this is a PR), and EXIT. The orchestrator's heartbeat owns merge-on-green.`,
    );
  }

  // OWNERSHIP NOT DETERMINABLE (cwd is the shared tree / not a stranded worktree — the common
  // case, since agents often `cd` into their worktree per-command rather than as a persistent
  // cwd). We must NOT block here — blocking on a worktree we can't attribute to this agent
  // could wedge the wrong one. Instead WARN (non-blocking): scan fray PR-worktrees and, if any
  // is unpushed, surface a heads-up so the agent/orchestrator notices, while still letting the
  // rest proceed. Pure fail-open: any uncertainty → allow.
  const porcelain = git(projectDir, ['worktree', 'list', '--porcelain']);
  if (!porcelain) allow();
  /** @type {string[]} */
  const warnings = [];
  let curPath = '';
  for (const line of porcelain.split('\n')) {
    if (line.startsWith('worktree ')) curPath = line.slice('worktree '.length).trim();
    else if (line === '' && curPath) {
      if (/(^|\/)(private\/)?tmp\/nub-wt-/.test(curPath)) {
        const s = strandedAt(curPath);
        if (s) warnings.push(`  • ${curPath} (branch ${s.branch}) is ${s.ahead} commit(s) ahead of its remote — UNPUSHED.`);
      }
      curPath = '';
    }
  }
  if (curPath && /(^|\/)(private\/)?tmp\/nub-wt-/.test(curPath)) {
    const s = strandedAt(curPath);
    if (s) warnings.push(`  • ${curPath} (branch ${s.branch}) is ${s.ahead} commit(s) ahead of its remote — UNPUSHED.`);
  }

  if (warnings.length === 0) allow();

  // Non-blocking heads-up: emit a systemMessage but ALLOW the rest. (Warn-not-block when we
  // can't attribute the worktree to the resting agent — preserves fail-open totality.)
  process.stdout.write(
    JSON.stringify({
      systemMessage:
        `⟦fray push-before-rest — heads-up (not blocking)⟧ A PR worktree has a committed-but-UNPUSHED change:\n` +
        warnings.join('\n') +
        `\nIf this is yours, push it before exiting (\`git push origin HEAD:<branch>\`). The orchestrator's heartbeat owns merge-on-green.`,
    }),
  );
  process.exit(0);
} catch {
  allow(); // fail open — a bug here must never wedge an agent
}
