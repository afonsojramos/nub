#!/usr/bin/env node
// ci-watch — block until a GitHub Actions run (or a PR's check rollup) is TRULY
// terminal, then exit with a status the orchestrator can trust. The robust
// replacement for `gh run watch`/`gh pr checks --watch`, which exit EARLY when
// armed right after a push (the run is QUEUED with no jobs registered yet, so gh
// sees "nothing in progress" and returns success) and also surface a non-zero
// exit on a TRANSIENT API error (a 401/5xx mid-watch reads as "the run failed").
//
// Runs under BOTH plain Node (type-stripping) and nub:
//   node scripts/ci-watch.ts --run <run-id>  [--repo o/r] [--timeout <min>]
//   nub  scripts/ci-watch.ts --pr  <number>  [--repo o/r] [--timeout <min>]
//   nub  scripts/ci-watch.ts --pr  <number>  --required "CI gate"   # gate on branch-protection checks
//   nub  scripts/ci-watch.ts --pr  <number>  --chunk[=<min>]        # foreground sub-agent loop
//
// Erasable TypeScript only (no enums/namespaces/parameter-properties) so plain
// modern `node` runs it with no build step — same constraint as the other
// scripts/*.ts.
//
// ORCHESTRATOR tooling — designed to run as a detached `run_in_background` task
// that re-invokes the orchestrator on exit. The final stdout line is a single
// self-describing summary (CI-WATCH …: SUCCESS/FAILURE/STUCK/TIMEOUT/ERROR) so
// the outcome is readable from the tail.
//
// Exit codes (the contract the orchestrator gates on):
//   0  completed AND all green
//   1  a check/job concluded FAILURE/CANCELLED/TIMED_OUT/STARTUP_FAILURE
//   2  required/named checks still NOT green after --timeout (genuinely stuck)
//   3  usage / target-unresolvable / unrecoverable error
//   4  STUCK-but-SAFE — every required/named check is GREEN, but a non-terminal
//      GHOST check (nameless / never-terminating) remains, so a strict "all
//      checks terminal" gate would hang forever. The caller reads this + the
//      summary and DECIDES (e.g. `gh pr merge --admin`) instead of the process
//      hanging on a check that never reports. See "the #327 ghost" below.
//
// The #327 ghost: GitHub occasionally registers a check-run that NEVER reports a
// status — it stays PENDING with no name forever. A watcher that waits for ALL
// rollup items to be terminal then hangs indefinitely even though every REAL
// check is green (PR #327 sat green-but-unmerged for hours this way). The fix:
// a nameless / never-terminating non-required check does NOT block a green
// verdict — it is surfaced as exit 4 (STUCK-but-safe), never waited on forever.
//
// Core fixes over the raw watchers:
//   * WAIT-FOR-EXISTENCE: a not-found / no-jobs-yet target is "keep polling",
//     never "done". This is the premature-exit fix.
//   * AUTHORITATIVE terminal check: done only when status == "completed" (run) /
//     every REQUIRED/named rollup item terminal+green (pr) — never inferred from
//     "nothing running".
//   * FAIL-FAST: exit non-zero the instant ANY job/check is a failure, without
//     waiting for the rest (mirrors the AGENTS.md fail-fast rule).
//   * NEVER-HANG: a ghost (nameless / stuck-pending non-required) check can
//     never park the watcher forever — a no-progress window converts it to an
//     actionable exit-4 verdict.
//   * TRANSIENT-ERROR TOLERANCE: a gh/API hiccup is retried with backoff, not
//     treated as a run failure.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// ---- args -------------------------------------------------------------------

type Mode = "run" | "pr";
type Opts = {
  mode: Mode;
  target: string;
  repo: string | null;
  timeoutMin: number;
  // --required: the branch-protection checks that actually gate merge. When set,
  // success fires the instant every named required check is terminal+green — any
  // other pending check (a ghost, or a non-required job) is non-blocking, so the
  // watcher matches branch-protection semantics and structurally cannot hang.
  required: Set<string>;
  // --no-progress: how long the incomplete-check set may sit UNCHANGED with all
  // required/named checks already green before the watcher gives up on the
  // remaining ghost(s) and exits 4 (STUCK-but-safe). Bounds the ghost wait.
  noProgressMin: number;
  // --chunk: per-invocation wall-clock cap for sub-agent foreground loops.
  // When set and the cap expires, exits 2 with a RERUN message instead of the
  // generic TIMEOUT message, signalling the agent to re-run the same command.
  chunkMin: number | null;
};

const HELP = `ci-watch — block until a CI run / PR check rollup is truly terminal

Usage:
  node scripts/ci-watch.ts --run <run-id> [flags]
  nub  scripts/ci-watch.ts --pr  <number> [flags]

Modes (exactly one):
  --run <run-id>     Watch a workflow run (gh run view).
  --pr  <number>     Watch a PR's check rollup (gh pr view).

Flags:
  --repo <owner/repo>   Repository (default: current repo from gh).
  --timeout <minutes>   Max wall-clock before giving up as pending (default 45).
  --required <names>    Comma-separated branch-protection check names to gate on
                        (e.g. --required "CI gate"). Success fires as soon as
                        every required check is green; a ghost or a non-required
                        check (pending OR failed) never blocks — branch
                        protection doesn't gate on it. The precise, hang-proof
                        gate for a merge watcher.
  --no-progress <min>   How long an UNCHANGED incomplete set (all required/named
                        checks already green, only a ghost remaining) may sit
                        before exiting 4 STUCK-but-safe (default 8).
  --chunk[=<minutes>]   Sub-agent foreground-loop mode (default 9 min). Caps each
                        invocation under the 10-min Bash tool timeout; exits 2
                        with a RERUN message when the cap expires so the agent
                        can loop. While exit 2 (pending): re-run the SAME command.
  -h, --help            Show this help.

Exit codes: 0 all green · 1 a check failed · 2 required/named not green after
            timeout · 3 usage/error · 4 STUCK-but-safe (required green, a ghost
            check will never terminate — safe to --admin merge).

Designed to run as a detached run_in_background task; the final stdout line is a
single CI-WATCH summary the orchestrator reads from the tail.

Sub-agent foreground-loop pattern (use when a sub-agent must gate on its own CI):
  # Bash tool: foreground (NOT run_in_background), timeout: 570000
  nub scripts/ci-watch.ts --pr <N> --chunk
  # exit 0 → green   1 → red, fix + re-push   2 → pending, re-run   3 → error
  # exit 4 → required green but a ghost check is stuck; decide (--admin merge)
  # While exit code is 2, call the same command again. Each chunk completes within
  # the Bash timeout cap so no call is ever killed mid-watch.`;

function die(msg: string): never {
  process.stderr.write(`ci-watch: ${msg}\n`);
  process.exit(3);
}

const CHUNK_DEFAULT_MIN = 9;
const NO_PROGRESS_DEFAULT_MIN = 8;

function parseArgs(argv: string[]): Opts {
  let mode: Mode | null = null;
  let target = "";
  let repo: string | null = null;
  let timeoutMin = 45;
  let noProgressMin = NO_PROGRESS_DEFAULT_MIN;
  let chunkMin: number | null = null;
  const required = new Set<string>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-h" || a === "--help") {
      process.stdout.write(HELP + "\n");
      process.exit(0);
    } else if (a === "--run") {
      if (mode) die("--run and --pr are mutually exclusive");
      mode = "run";
      target = argv[++i] ?? die("--run requires a run-id");
    } else if (a === "--pr") {
      if (mode) die("--run and --pr are mutually exclusive");
      mode = "pr";
      target = argv[++i] ?? die("--pr requires a number");
    } else if (a === "--repo") {
      repo = argv[++i] ?? die("--repo requires owner/repo");
    } else if (a === "--required") {
      const csv = argv[++i] ?? die("--required requires a check name (or comma-separated list)");
      for (const n of csv.split(",").map((s) => s.trim()).filter(Boolean)) required.add(n);
      if (required.size === 0) die("--required was given no non-empty check name");
    } else if (a === "--timeout") {
      timeoutMin = Number(argv[++i]);
      if (!Number.isFinite(timeoutMin) || timeoutMin <= 0) die("--timeout must be a positive number of minutes");
    } else if (a === "--no-progress") {
      noProgressMin = Number(argv[++i]);
      if (!Number.isFinite(noProgressMin) || noProgressMin <= 0) die("--no-progress must be a positive number of minutes");
    } else if (a === "--chunk") {
      // bare --chunk: use the next arg as the value only if it looks like a number
      const next = argv[i + 1];
      if (next !== undefined && /^\d+(\.\d+)?$/.test(next)) {
        chunkMin = Number(next);
        i++;
      } else {
        chunkMin = CHUNK_DEFAULT_MIN;
      }
      if (!Number.isFinite(chunkMin) || chunkMin <= 0) die("--chunk requires a positive number of minutes");
    } else if (a.startsWith("--chunk=")) {
      // --chunk=<N> form
      chunkMin = Number(a.slice("--chunk=".length));
      if (!Number.isFinite(chunkMin) || chunkMin <= 0) die("--chunk= requires a positive number of minutes");
    } else {
      die(`unknown arg: ${a} (try --help)`);
    }
  }
  if (!mode) die("specify --run <run-id> or --pr <number>");
  return { mode, target, repo, timeoutMin, required, noProgressMin, chunkMin };
}

// ---- gh plumbing ------------------------------------------------------------

// A gh call that may transiently fail (network blip, 401 token refresh, 5xx).
// Returns the stdout on success, or null on failure — the caller decides whether
// a null is "keep polling" (transient / not-yet-existing) or fatal. We never let
// a single failed gh call abort the watch.
function ghTry(args: string[]): string | null {
  try {
    return execFileSync("gh", args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }).trim();
  } catch {
    return null;
  }
}

function repoArgs(repo: string | null): string[] {
  return repo ? ["--repo", repo] : [];
}

// gh uses its stored auth token implicitly for every call above, which gives the
// authenticated (high) rate limit for free. We surface a one-time warning if no
// token is resolvable and stretch the backoff so an unauthenticated fallback
// stays well under the lower anonymous limit.
function hasAuthToken(): boolean {
  const t = ghTry(["auth", "token"]);
  return t !== null && t.length > 0;
}

// ---- terminal-state classification ------------------------------------------

const FAILURE_CONCLUSIONS = new Set(["FAILURE", "CANCELLED", "TIMED_OUT", "STARTUP_FAILURE", "ACTION_REQUIRED", "STALE"]);
const OK_CONCLUSIONS = new Set(["SUCCESS", "NEUTRAL", "SKIPPED"]);

type RollupItem = { name?: string; context?: string; status?: string; conclusion?: string; state?: string; startedAt?: string };

// A rollup item is either a CheckRun (name/status/conclusion) or a StatusContext
// (context/state) — distinguished by which fields gh populated, and their display
// names live in DIFFERENT fields: CheckRun.name vs StatusContext.context. Reading
// only `.name` (the pre-hardening bug) rendered every legacy status as "(unnamed)".
function itemName(it: RollupItem): string {
  if (it.name) return it.name;
  if (it.context) return it.context;
  return "";
}

// { terminal, failed } for one item. A non-terminal item is neither. A COMPLETED
// CheckRun with an empty conclusion is treated as non-failing (matches the prior
// classifier: only a KNOWN-bad conclusion fails); an item with neither status nor
// state is treated as a non-terminal ghost rather than a failure.
function itemState(it: RollupItem): { terminal: boolean; failed: boolean } {
  if (it.status !== undefined) {
    if ((it.status || "").toUpperCase() !== "COMPLETED") return { terminal: false, failed: false };
    const c = (it.conclusion || "").toUpperCase();
    if (c === "" || OK_CONCLUSIONS.has(c)) return { terminal: true, failed: false };
    return { terminal: true, failed: true };
  }
  if (it.state !== undefined) {
    const s = (it.state || "").toUpperCase();
    if (s === "" || s === "PENDING") return { terminal: false, failed: false };
    if (s === "SUCCESS") return { terminal: true, failed: false };
    return { terminal: true, failed: true };
  }
  return { terminal: false, failed: false };
}

// The partition that drives every verdict. GHOSTS are the never-hang carve-out:
// a non-terminal check that either has NO NAME (the #327 ghost) or — when a
// --required set is supplied — is simply not one of the required checks. Neither
// blocks a green verdict; both are surfaced, never waited on forever. REAL-
// PENDING are the named checks that genuinely gate (in --required mode, only the
// required ones), and DO block until terminal.
type Buckets = {
  failures: string[];
  realPending: string[];
  ghosts: string[];
  greenNamed: number;
  total: number;
  requiredMissing: string[]; // populated only in --required mode
};

function classifyRollup(rollup: RollupItem[], required: Set<string>): Buckets {
  const scoped = required.size > 0;
  const failures: string[] = [];
  const realPending: string[] = [];
  const ghosts: string[] = [];
  let greenNamed = 0;
  for (const it of rollup) {
    const name = itemName(it);
    const st = itemState(it);
    // In --required mode a NON-required check never blocks — pass OR fail. Branch
    // protection doesn't gate on it, so a red optional check must not report
    // FAILURE (that would refuse to merge a mergeable PR). It is non-blocking;
    // a non-terminal one is surfaced as a ghost, a terminal one is ignored.
    if (scoped && !required.has(name)) {
      if (!st.terminal) ghosts.push(name || "(unnamed)");
      continue;
    }
    if (st.terminal) {
      if (st.failed) failures.push(name || "(unnamed)");
      else if (name) greenNamed++;
      continue;
    }
    // Non-terminal required-or-any check. Nameless → ghost (the #327 ghost);
    // named → a real pending check that genuinely gates.
    if (name === "") ghosts.push("(unnamed)");
    else realPending.push(name);
  }
  // A required check is satisfied only when EVERY occurrence of its name is
  // terminal+green — a matrix / re-run can list the same name twice, and branch
  // protection keys on the latest, so the first-match `find` would green-light a
  // still-pending same-named check.
  const requiredMissing: string[] = [];
  if (scoped) {
    for (const rname of required) {
      const matches = rollup.filter((it) => itemName(it) === rname);
      const allGreen = matches.length > 0 && matches.every((it) => { const st = itemState(it); return st.terminal && !st.failed; });
      if (!allGreen) requiredMissing.push(rname);
    }
  }
  return { failures, realPending, ghosts, greenNamed, total: rollup.length, requiredMissing };
}

// ghostsOnly marks the STUCK-but-safe shape: no real/required check is pending,
// yet a ghost remains non-terminal. The loop converts a persistent ghostsOnly
// state into exit 4 rather than waiting forever.
type Verdict =
  | { kind: "pending"; reason: string; ghostsOnly: boolean; realPending: string[]; ghosts: string[]; greenNamed: number }
  | { kind: "success"; reason: string }
  | { kind: "failure"; reason: string };

function joinCapped(names: string[], n: number): string {
  return `${names.slice(0, n).join(", ")}${names.length > n ? " …" : ""}`;
}

function verdictForBuckets(b: Buckets, hasRequired: boolean): Verdict {
  if (b.failures.length > 0) return { kind: "failure", reason: `failing check(s): ${joinCapped(b.failures, 4)}` };
  if (b.total === 0) return { kind: "pending", reason: "no checks registered yet", ghostsOnly: false, realPending: [], ghosts: [], greenNamed: 0 };
  if (hasRequired) {
    if (b.requiredMissing.length > 0)
      return { kind: "pending", reason: `required check(s) not green: ${joinCapped(b.requiredMissing, 4)}`, ghostsOnly: false, realPending: b.requiredMissing, ghosts: b.ghosts, greenNamed: b.greenNamed };
    return { kind: "success", reason: `all ${b.greenNamed} required check(s) green (of ${b.total} total; non-required checks non-blocking)` };
  }
  if (b.realPending.length > 0)
    return { kind: "pending", reason: `${b.realPending.length} check(s) pending: ${joinCapped(b.realPending, 4)}`, ghostsOnly: false, realPending: b.realPending, ghosts: b.ghosts, greenNamed: b.greenNamed };
  if (b.ghosts.length > 0)
    return { kind: "pending", reason: `${b.greenNamed} named check(s) green; ${b.ghosts.length} non-terminal ghost check(s) that may never report: ${joinCapped(b.ghosts, 4)}`, ghostsOnly: true, realPending: [], ghosts: b.ghosts, greenNamed: b.greenNamed };
  return { kind: "success", reason: `${b.total} check(s) green` };
}

function classifyPr(json: string, required: Set<string>): Verdict {
  let d: { statusCheckRollup?: RollupItem[] };
  try {
    d = JSON.parse(json);
  } catch {
    return { kind: "pending", reason: "unparseable PR JSON (transient)", ghostsOnly: false, realPending: [], ghosts: [], greenNamed: 0 };
  }
  return verdictForBuckets(classifyRollup(d.statusCheckRollup || [], required), required.size > 0);
}

// A run is done only when its top-level status is "completed". Until then —
// including QUEUED with zero jobs (the premature-exit case) — it is pending.
// Fail-fast: a failed job short-circuits to failure without waiting for siblings.
// Jobs always carry names, so the ghost carve-out does not apply to run mode.
function classifyRun(json: string): Verdict {
  let d: { status?: string; conclusion?: string; jobs?: { name?: string; status?: string; conclusion?: string }[] };
  try {
    d = JSON.parse(json);
  } catch {
    return { kind: "pending", reason: "unparseable run JSON (transient)", ghostsOnly: false, realPending: [], ghosts: [], greenNamed: 0 };
  }
  const jobs = d.jobs || [];
  for (const j of jobs) {
    if ((j.status || "").toLowerCase() === "completed") {
      const c = (j.conclusion || "").toUpperCase();
      if (FAILURE_CONCLUSIONS.has(c)) return { kind: "failure", reason: `job "${j.name || "?"}" → ${c}` };
    }
  }
  if ((d.status || "").toLowerCase() !== "completed") {
    const running = jobs.filter((j) => (j.status || "").toLowerCase() !== "completed").map((j) => j.name || "?");
    return { kind: "pending", reason: jobs.length === 0 ? "no jobs registered yet (queued)" : `${running.length}/${jobs.length} job(s) still running`, ghostsOnly: false, realPending: running, ghosts: [], greenNamed: jobs.length - running.length };
  }
  const c = (d.conclusion || "").toUpperCase();
  if (OK_CONCLUSIONS.has(c)) return { kind: "success", reason: `${jobs.length} job(s) green (${c})` };
  return { kind: "failure", reason: `run concluded ${c || "no-conclusion"}` };
}

// ---- pending-state timing (pure; unit-tested) -------------------------------

// The incomplete-set fingerprint. When it stops changing while the verdict is
// ghostsOnly, nothing more will ever happen — that is the signal to stop waiting.
function signatureOf(v: Verdict): string {
  if (v.kind !== "pending") return v.kind;
  return JSON.stringify([[...v.realPending].sort(), [...v.ghosts].sort(), v.greenNamed]);
}

type Timing = { lastSig: string | null; lastProgressAt: number };

// Decide whether the watch may keep waiting or must exit now with an actionable
// verdict — the anti-hang core. Returns null to keep polling, or a terminal
// {code, summary}. Kept pure (no gh, no clock, no sleep) so the #327 ghost shape
// is unit-testable without real time or network.
//
// `v` is the LAST pending verdict, or null when no successful poll has produced
// one yet (a gh-failure streak from the start). Called EVERY iteration, including
// on a failed poll — the deadline/chunk caps must fire even during a transient-gh
// streak, or a `--chunk` invocation can blow past the Bash-tool timeout and be
// killed mid-watch.
//   exit 4 — ghostsOnly persisted past the no-progress window OR the overall
//            deadline hit with only ghosts left: required/named green, safe.
//   exit 2 — chunk cap hit (RERUN) or overall deadline hit with a REAL/required
//            check still pending (or no status ever resolved): NOT safe to merge.
function resolvePendingExit(
  v: (Verdict & { kind: "pending" }) | null,
  label: string,
  now: number,
  timing: Timing,
  cfg: { deadline: number; chunkDeadline: number | null; noProgressMs: number; chunkMin: number | null; timeoutMin: number },
): { code: number; summary: string } | null {
  const stuckSafe = () => ({
    code: 4,
    summary: `CI-WATCH ${label}: STUCK — required/named checks GREEN (${v ? v.greenNamed : 0}), ${v ? v.ghosts.length : 0} non-terminal ghost/non-required check(s): ${joinCapped(v ? v.ghosts : [], 4)}; safe to --admin merge`,
  });

  // A ghost that will never report must not park the watcher forever: once the
  // incomplete set has been UNCHANGED for the no-progress window (all real checks
  // already green), stop and surface it. Progress (a new named check registering)
  // resets lastProgressAt in the caller, so this only fires on a genuine stall.
  if (v && v.ghostsOnly && now - timing.lastProgressAt >= cfg.noProgressMs) return stuckSafe();

  // Chunk cap: sub-agent foreground loop — exit 2 with a RERUN message so it loops.
  if (cfg.chunkDeadline !== null && now > cfg.chunkDeadline) {
    return { code: 2, summary: `CI-WATCH ${label}: PENDING after ${cfg.chunkMin}m — RERUN the SAME command to continue` };
  }

  // Overall deadline: only-ghosts-left is STUCK-but-safe (exit 4); a real/required
  // check still pending (or nothing resolved) after the full timeout is exit 2.
  if (now > cfg.deadline) {
    if (v && v.ghostsOnly) return stuckSafe();
    const pending = v && v.realPending.length > 0 ? joinCapped(v.realPending, 4) : "no check status resolved";
    return { code: 2, summary: `CI-WATCH ${label}: TIMEOUT — required/named check(s) NOT green after ${cfg.timeoutMin}min: ${pending}` };
  }
  return null;
}

// ---- poll loop --------------------------------------------------------------

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// Exponential backoff with jitter: 10s → 20s → 40s → cap. authenticated caps at
// 60s; unauthenticated stretches to 90s to stay under the anonymous rate limit.
function nextDelay(prev: number, cap: number): number {
  const grown = Math.min(prev * 2, cap);
  const jitter = grown * 0.2 * (Math.random() - 0.5); // ±10%
  return Math.round(grown + jitter);
}

async function watch(opts: Opts): Promise<{ code: number; summary: string }> {
  const label = opts.mode === "run" ? `run ${opts.target}` : `pr ${opts.target}`;
  const authed = hasAuthToken();
  if (!authed) process.stderr.write("ci-watch: no gh auth token resolvable — falling back to slower polling to respect the anonymous rate limit\n");

  const viewArgs =
    opts.mode === "run"
      ? ["run", "view", opts.target, ...repoArgs(opts.repo), "--json", "status,conclusion,jobs"]
      : ["pr", "view", opts.target, ...repoArgs(opts.repo), "--json", "statusCheckRollup,mergeable,mergeStateStatus"];
  const classify = opts.mode === "run" ? (out: string) => classifyRun(out) : (out: string) => classifyPr(out, opts.required);

  const cap = authed ? 60_000 : 90_000;
  const deadline = Date.now() + opts.timeoutMin * 60_000;
  const chunkDeadline = opts.chunkMin !== null ? Date.now() + opts.chunkMin * 60_000 : null;
  const noProgressMs = opts.noProgressMin * 60_000;
  const cfg = { deadline, chunkDeadline, noProgressMs, chunkMin: opts.chunkMin, timeoutMin: opts.timeoutMin };
  let delay = 10_000;
  let consecutiveErrors = 0;
  // Tracks whether the incomplete-check set is still changing — a stuck ghost is
  // only "given up on" after the set has been unchanged for the no-progress window.
  const timing: Timing = { lastSig: null, lastProgressAt: Date.now() };
  // The last pending verdict a successful poll produced. Retained so the deadline
  // check below can run — and message accurately — even on an iteration whose poll
  // failed transiently (null until the first successful poll).
  let lastPending: (Verdict & { kind: "pending" }) | null = null;

  for (;;) {
    const out = ghTry(viewArgs);
    if (out === null) {
      // gh call failed: target may not exist YET (just pushed) or a transient
      // API error. Either way → keep polling. Never treat as completion.
      consecutiveErrors++;
      if (consecutiveErrors >= 12) {
        return { code: 3, summary: `CI-WATCH ${label}: ERROR — gh unreachable / target unresolvable after ${consecutiveErrors} attempts` };
      }
      process.stderr.write(`    … gh call failed (attempt ${consecutiveErrors}); target not visible yet or transient — retrying\n`);
    } else {
      consecutiveErrors = 0;
      const v = classify(out);
      if (v.kind === "success") return { code: 0, summary: `CI-WATCH ${label}: SUCCESS (${v.reason})` };
      if (v.kind === "failure") {
        const url = ghTry(opts.mode === "run" ? ["run", "view", opts.target, ...repoArgs(opts.repo), "--json", "url", "--jq", ".url"] : ["pr", "view", opts.target, ...repoArgs(opts.repo), "--json", "url", "--jq", ".url"]);
        return { code: 1, summary: `CI-WATCH ${label}: FAILURE — ${v.reason}${url ? ` (${url})` : ""}` };
      }
      // pending: record it and mark progress when the incomplete set changes.
      lastPending = v;
      const sig = signatureOf(v);
      if (sig !== timing.lastSig) {
        timing.lastSig = sig;
        timing.lastProgressAt = Date.now();
      }
      process.stderr.write(`    … ${v.reason}\n`);
    }

    // Deadline/ghost check runs EVERY iteration (even after a failed poll) so the
    // overall timeout and the --chunk cap always fire on schedule.
    const exit = resolvePendingExit(lastPending, label, Date.now(), timing, cfg);
    if (exit) return exit;

    await sleep(delay);
    delay = nextDelay(delay, cap);
  }
}

async function main(): Promise<void> {
  const opts = parseArgs(process.argv.slice(2));
  const { code, summary } = await watch(opts);
  // The final stdout line IS the handoff — orchestrator reads it from the tail.
  console.log(summary);
  process.exit(code);
}

// Run main() only when invoked directly; when imported by a test, expose the pure
// classifiers/resolver without side effects.
const isMain = process.argv[1] !== undefined && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) main();

export { classifyRollup, verdictForBuckets, classifyPr, classifyRun, signatureOf, resolvePendingExit };
export type { Buckets, Verdict, RollupItem, Timing };
