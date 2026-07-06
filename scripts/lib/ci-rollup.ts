// ci-rollup — the shared #327 ghost-carve-out primitives for classifying a GitHub
// PR check rollup, used by BOTH scripts/ci-watch.ts (the single-PR watcher) and
// scripts/merge-cascade.ts (the queue-drain merger). ONE source of truth for
// "is this check a ghost" and "are the required (or all named) checks green,
// ignoring ghosts" — so the two tools cannot drift on the merge-safety verdict.
//
// The #327 ghost: GitHub occasionally registers a check-run that NEVER reports a
// status — it stays non-terminal with no name forever. A gate that waits for
// ALL rollup items to be terminal then hangs indefinitely even though every REAL
// check is green (PR #327 sat green-but-unmerged for hours this way). The carve-
// out: a nameless / never-terminating non-required check does NOT block a green
// verdict — it is a ghost, surfaced but never waited on forever.
//
// Erasable TypeScript only (no enums/namespaces/parameter-properties) so plain
// modern `node` type-strips it with no build step — same constraint as its
// importers.

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
  requiredMissing: string[]; // populated only when a required set is supplied
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
// yet a ghost remains non-terminal. A single-PR watcher converts a persistent
// ghostsOnly state into an actionable exit rather than waiting forever; a merger
// that positively gates on the required set treats it as merge-ready.
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

export { FAILURE_CONCLUSIONS, OK_CONCLUSIONS, itemName, itemState, classifyRollup, joinCapped, verdictForBuckets };
export type { RollupItem, Buckets, Verdict };
