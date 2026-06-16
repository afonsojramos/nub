#!/usr/bin/env node
// @ts-check
// UserPromptSubmit hook — injects the DYNAMIC, per-turn orchestrator pulse each turn.
// Run directly with node: `node .claude/hooks/fray-reminder.mjs` (no transpiler — max
// Node compat; fray's own hooks never depend on nub to run).
//
// Carries ONLY the genuinely-dynamic, anti-drop bits that can change turn-to-turn:
//   - the `modeLine` (autonomous vs interactive + the autonomous empowerment paragraph),
//     gated on `autonomousMode` (which can flip mid-session); and
//   - the `status` (pending-thread list BY NAME + per-message frontmatter validation +
//     the drain-queue / decided≠open / reconcile one-liner).
// The STATIC orchestrator role + hygiene doctrine (formerly the `core` const, ~379 tok)
// moved to the once-per-session SessionStart hook (.claude/hooks/session-seed.mjs) — it
// does not change within a session, so re-paying it every prompt was pure waste.
// Emits hookSpecificOutput.additionalContext (model-only). Robust: never throws (a broken
// hook must not disrupt the prompt) — any failure → inject nothing.
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadConfig, STATUS, TERMINAL } from '../../scripts/fray/config.mjs';

// Token-saving: skip entirely inside sub-agent contexts. The hook stdin carries
// `agent_id` ONLY when fired inside a sub-agent (UserPromptSubmit shouldn't fire there
// at all, so this is belt-and-suspenders). Main session → no agent_id → proceed.
try {
  const hi = JSON.parse(readFileSync(0, 'utf8'));
  if (hi.agent_id ?? hi.agentId) process.exit(0);
} catch {
  /* no stdin / not JSON → assume main session, proceed */
}

/**
 * Emit the model-only additionalContext and exit.
 * @param {string} ctx
 * @returns {never}
 */
function emit(ctx) {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: 'UserPromptSubmit', additionalContext: ctx },
    }),
  );
  process.exit(0);
}

try {
  const dir = process.env.CLAUDE_PROJECT_DIR ?? '.';
  // autonomous_mode + the kill-switch live in .fray/config.yml — parsed by the shared,
  // type-safe loadConfig. The board/status view is COMPUTED by the tool, never stored.
  const cfg = loadConfig(dir);
  if (cfg.enabled === false) process.exit(0); // fray kill-switch — `enabled: false` silences the hook (missing/unparseable → defaults → enabled, fail-safe).
  const mode = cfg.autonomousMode ? 'on' : 'off';

  // fray: thread pulse + per-message frontmatter VALIDATION (so a malformed thread
  // surfaces immediately, not whenever I happen to look). STATUS/TERMINAL come from the
  // shared module — same source the tool's `--validate` uses. Unrecognized fields are
  // allowed by design — only required fields + the status vocab are checked.
  /** @type {string[]} */
  const pending = []; // `<slug>[status]` for every non-terminal thread — compact, one line, names included so a stalled thread is caught BY NAME (not just a count). Full detail stays in `node scripts/fray/index.mjs`, NOT injected per-message.
  /** @type {string[]} */
  const queued = []; // non-terminal threads that still carry a `QUEUED` follow-up marker — surfaced BY NAME so the drain-the-queue step can't be skipped (missing these is totally unacceptable).
  /** @type {string[]} */
  const errors = [];
  try {
    for (const f of readdirSync(join(dir, '.fray'))) {
      if (!f.endsWith('.md') || f.startsWith('_')) continue; // `_`-prefixed = non-thread meta
      const id = f.replace(/\.md$/, '');
      const src = readFileSync(join(dir, '.fray', f), 'utf8');
      const st = src.match(/^status:\s*(\S+)/m)?.[1];
      if (!/^title:\s*\S/m.test(src)) errors.push(`${id}: missing title`);
      if (!st) errors.push(`${id}: missing status`);
      else if (!STATUS.includes(st)) errors.push(`${id}: invalid status "${st}"`);
      if (!TERMINAL.includes(st ?? '')) {
        pending.push(`${id}[${st ?? '?'}]`);
        if (/\bQUEUED\b/.test(src)) queued.push(id);
      }
    }
  } catch {
    /* no .fray dir yet */
  }
  const status =
    `FRAY — ${pending.length} pending: ${pending.join(', ') || 'none'}. Advance or reconcile EACH this turn; if you went deep on ONE thread, don't let the others silently stall (\`node scripts/fray/index.mjs\` for detail). When you fold a return: DRAIN that thread's queued follow-ups (\`## Steps\` items marked QUEUED — dispatch on <agent>'s return) + MOVE any answered Open question into Decisions (a DECIDED thing lives under ## Decisions, NEVER Open questions). done/dismissed threads are TERMINAL + KEPT — never delete them. NO IN-FLIGHT STEERING: you cannot message a running agent in this harness — to add context / redirect / fold extra scope into a file an agent already owns, mark it \`enqueued\` (naming the agent it waits on) and dispatch the instant that agent returns; never spawn a clobbering sibling or kill-and-respawn.` +
    (queued.length ? `  ⚠ UN-DRAINED QUEUED FOLLOW-UPS in ${queued.length} thread(s): ${queued.join(', ')}. THE INSTANT any of their agents returns, RE-READ that thread .md (don't reconcile from memory) and DISPATCH the actionable QUEUED items as sub-agents THIS turn (a mandated self-review/integration pass IS one — dispatch it). Surface, never silently drop, the human-gated/post-launch ones. Skipping this is the #1 failure.` : '') +
    (errors.length ? `  ⚠ VALIDATION ERRORS (fix now): ${errors.join('; ')}` : '');

  const modeLine =
    mode === 'on'
      ? "AUTONOMOUS MODE = ON (the human is away). What this MEANS: MAKE REASONABLE DECISIONS WITHOUT A HUMAN IN THE LOOP — do NOT ask questions, do NOT stall for confirmation; bias HARD to action and keep the background fleet busy. At a fork, pick the sensible option, DOCUMENT the call in the tracker, and PROCEED (choosing intelligently and letting the maintainer adjust on review beats stopping). Reconcile every completed sub-agent and immediately dispatch the next work. The ONLY things you may NOT autonomously land: a default / security-posture / product / brand / API-config-env decision the maintainer owns (recommend-only — design+prototype, surface to the tracker's decisions queue, don't flip the default), anything irreversible/destructive/published-external, and parked work not yet greenlit. Everything else: decide and do. Scan the board via `node scripts/fray/index.mjs`. You ARE empowered — CONTINUOUSLY cut patch releases (0.0.x: make version → commit → tag → push), push main, create repos, install tooling (brew), set up VMs, land greenlit work — yourself, no asking. Do NOT build an 'awaiting-maintainer' queue from REVERSIBLE decisions (the #1 repeated correction). The only true gates: a 0.1.0 bump, a truly-irreversible-destructive act, a user-facing DEFAULT (ship behind a flag, don't freeze), and public marketing wording."
      : `autonomous_mode=${mode} → interactive: surface decisions + ask rather than auto-landing.`;

  emit(`${modeLine}  ${status}`);
} catch {
  // Fail-open: a broken hook must not disrupt the prompt. The static doctrine now lives in
  // SessionStart (session-seed.mjs), so there is nothing safe to fall back to here — inject
  // nothing rather than risk emitting partial/garbage.
  process.exit(0);
}
