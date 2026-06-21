#!/usr/bin/env node
// @ts-check
/**
 * fray — merge-queue helper. Landing agents push-then-EXIT and enqueue a PR here; the
 * orchestrator's heartbeat drains it (poll CI → squash-merge on green → remove). This
 * module makes the queue ROBUST where the prose drain was fragile:
 *   - DEDUP BY PR — a re-push (red→fix→re-push) must not leave two lines for one PR;
 *   - REMOVE MERGED-OR-CLOSED entries — not just "merged by us"; a PR merged/closed by any
 *     route (manual merge, closed-without-merge, superseded) must drop out, or it sticks
 *     in the queue forever;
 *   - ATOMIC writes — write to a temp file + rename, so a concurrent reader never sees a
 *     half-written queue.
 *
 * Usage:
 *   node scripts/fray/merge-queue.mjs enqueue --pr 36 --sha 8cc0f33 --branch X --thread Y
 *   node scripts/fray/merge-queue.mjs list                 # print current entries (JSON lines)
 *   node scripts/fray/merge-queue.mjs drain                # remove merged/closed + dedup; print what to act on
 *
 * `drain` queries `gh pr view <pr> --json state` for each DEDUPED entry:
 *   - state MERGED or CLOSED → drop the entry (work is done / abandoned);
 *   - state OPEN             → keep the entry, and emit it to stdout as actionable
 *                             (the heartbeat then checks CI and merges on green).
 * It REWRITES the queue with only the still-open, deduped entries. `gh` errors fail-safe:
 * an entry whose state can't be determined is KEPT (never silently dropped).
 *
 * FAIL-SAFE: a missing queue file is empty (no error). Malformed lines are skipped on read.
 * This is an orchestrator-side maintenance tool, NOT a hook — it may call `gh` (network).
 */
import { readFileSync, writeFileSync, renameSync, existsSync, mkdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { execFileSync } from 'node:child_process';

const PROJECT_DIR = process.env.CLAUDE_PROJECT_DIR || process.cwd();
const QUEUE_PATH = join(PROJECT_DIR, '.fray', 'merge-queue.jsonl');

/**
 * Read + parse the queue, skipping malformed lines. Missing file → [].
 * @returns {Record<string, any>[]}
 */
export function readQueue() {
  let raw;
  try {
    raw = readFileSync(QUEUE_PATH, 'utf8');
  } catch {
    return []; // no file → empty queue
  }
  /** @type {Record<string, any>[]} */
  const out = [];
  for (const line of raw.split('\n')) {
    const t = line.trim();
    if (!t) continue;
    try {
      out.push(JSON.parse(t));
    } catch {
      /* skip a malformed line */
    }
  }
  return out;
}

/**
 * DEDUP by PR number, keeping the LAST occurrence (the freshest enqueue for that PR).
 * Entries with no usable `pr` are kept as-is (keyed by a unique sentinel so they survive).
 * @param {Record<string, any>[]} entries
 * @returns {Record<string, any>[]}
 */
export function dedupeByPr(entries) {
  /** @type {Map<string, Record<string, any>>} */
  const byPr = new Map();
  let n = 0;
  for (const e of entries) {
    const key = e && e.pr != null ? `pr:${e.pr}` : `noPr:${n++}`;
    byPr.set(key, e); // later wins → freshest enqueue for a PR
  }
  return [...byPr.values()];
}

/**
 * Atomic write of the queue (temp file + rename). Creates `.fray/` if needed.
 * @param {Record<string, any>[]} entries
 */
export function writeQueue(entries) {
  try {
    mkdirSync(dirname(QUEUE_PATH), { recursive: true });
  } catch {
    /* already exists */
  }
  const body = entries.map((e) => JSON.stringify(e)).join('\n') + (entries.length ? '\n' : '');
  const tmp = `${QUEUE_PATH}.tmp.${process.pid}`;
  writeFileSync(tmp, body);
  renameSync(tmp, QUEUE_PATH); // atomic on the same filesystem
}

/**
 * Append one entry, then dedup-by-PR + atomic-write. A re-push for an already-queued PR
 * collapses to a single (freshest) line rather than a duplicate.
 * @param {Record<string, any>} entry
 */
export function enqueue(entry) {
  const entries = dedupeByPr([...readQueue(), entry]);
  writeQueue(entries);
}

/**
 * `gh` PR state, uppercased (OPEN | MERGED | CLOSED), or null if it can't be determined.
 * @param {number|string} pr
 * @returns {string|null}
 */
function prState(pr) {
  try {
    const out = execFileSync('gh', ['pr', 'view', String(pr), '--json', 'state', '--jq', '.state'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 15000,
    }).trim();
    return out ? out.toUpperCase() : null;
  } catch {
    return null; // can't determine → caller keeps the entry (fail-safe)
  }
}

/**
 * Drain: dedup, then drop MERGED/CLOSED entries (work done/abandoned); keep OPEN and
 * indeterminate ones. Rewrites the queue atomically. Returns the entries still actionable
 * (OPEN), which the heartbeat then CI-checks + merges on green.
 * @returns {Record<string, any>[]} still-open (actionable) entries
 */
export function drain() {
  const deduped = dedupeByPr(readQueue());
  /** @type {Record<string, any>[]} */
  const keep = [];
  /** @type {Record<string, any>[]} */
  const open = [];
  for (const e of deduped) {
    if (e.pr == null) {
      keep.push(e); // no PR to check → keep (can't prove it's done)
      continue;
    }
    const state = prState(e.pr);
    if (state === 'MERGED' || state === 'CLOSED') continue; // done/abandoned → drop
    keep.push(e); // OPEN or indeterminate → keep
    if (state === 'OPEN') open.push(e);
  }
  writeQueue(keep);
  return open;
}

// CLI entrypoint (only when run directly, not when imported).
if (import.meta.url === `file://${process.argv[1]}`) {
  const [cmd, ...rest] = process.argv.slice(2);
  /** @param {string} flag */
  const arg = (flag) => {
    const i = rest.indexOf(`--${flag}`);
    return i >= 0 ? rest[i + 1] : undefined;
  };
  if (cmd === 'enqueue') {
    const pr = arg('pr');
    enqueue({
      pr: pr != null ? Number(pr) : undefined,
      sha: arg('sha'),
      branch: arg('branch'),
      thread: arg('thread'),
      enqueued_at: new Date().toISOString(),
    });
    process.stdout.write(`enqueued PR #${pr ?? '?'} (deduped, atomic)\n`);
  } else if (cmd === 'list') {
    for (const e of readQueue()) process.stdout.write(JSON.stringify(e) + '\n');
  } else if (cmd === 'drain') {
    const open = drain();
    process.stdout.write(
      open.length
        ? `${open.length} OPEN PR(s) to CI-check + merge-on-green:\n` + open.map((e) => `  #${e.pr} (${e.branch ?? '?'}) [${e.thread ?? '?'}]`).join('\n') + '\n'
        : 'merge-queue drained: no open PRs awaiting merge.\n',
    );
  } else {
    process.stderr.write('usage: merge-queue.mjs <enqueue|list|drain> [--pr N --sha S --branch B --thread T]\n');
    process.exit(existsSync(QUEUE_PATH) || cmd == null ? 1 : 1);
  }
}
