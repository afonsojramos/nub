#!/usr/bin/env node
// @ts-check
// UserPromptSubmit hook — injects a model-only orchestrator reminder each turn.
// Run directly with node: `node .claude/hooks/iw-reminder.mjs` (no transpiler — max
// Node compat; fray's own hooks never depend on nub to run).
//
// DYNAMIC: reads `autonomousMode` from the shared fray config loader (the IW
// structured-state store) so autonomous-specific nudges fire only when actually in
// autonomous mode, and scans the per-thread frontmatter to give a high-level board
// pulse + validation. Emits hookSpecificOutput.additionalContext (model-only).
// Robust: any failure → just the always-applicable core reminder; never throws (a
// broken hook must not disrupt the prompt).
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

const core =
  '⟦orchestrator reminder⟧ You are the ORCHESTRATOR: delegate ALL project work — code/doc edits, GitHub writes (comments/PR edits/resolves), builds, tests, investigations — to BACKGROUND sub-agents; never do them yourself in the foreground. Your foreground = dispatch, synthesize returns, decide, and edit your own control surfaces (the fray board/threads + memory/skill/settings) + final reviewed git. Keep the fray threads (.fray/<thread>.md; globals in .fray/config.yml) synced THIS turn: fold every returned sub-agent\'s facts into its thread, advance its status, surface decisions/questions; scan the board on demand (`node scripts/fray/index.mjs`). HYGIENE: keep each thread\'s ## Status + ## Next current so the LIVE state isn\'t buried — but a thread CAN hold a full record (a done/dismissed thread SHOULD have a complete investigation write-up; do NOT wipe detail to keep it lean). Global structured state lives in config.yml. DONE/DISMISSED threads are KEPT, NEVER deleted — each is its own file, excluded from the active board + the pending list by status, so a finished thread is zero bloat (a core benefit of per-file threads; do NOT clean them up). ONLY the orchestrator edits the board + thread files (sub-agents write findings sidecars, never the canonical docs). Reconcile EVERY in-flight sub-agent; never drop a thread. Before asserting how nub/aube is STRUCTURED, ground it in wiki/architecture.md / the nub-aube-architecture memory / code you just read — never reason from stale or secondhand framing.';

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
      if (!TERMINAL.includes(st ?? '')) pending.push(`${id}[${st ?? '?'}]`);
    }
  } catch {
    /* no .fray dir yet */
  }
  const status =
    `FRAY — ${pending.length} pending: ${pending.join(', ') || 'none'}. Advance or reconcile EACH this turn; if you went deep on ONE thread, don't let the others silently stall (\`node scripts/fray/index.mjs\` for detail). When you fold a return: DRAIN that thread's queued follow-ups (\`## Steps\` items marked QUEUED — dispatch on <agent>'s return) + MOVE any answered Open question into Decisions (a DECIDED thing lives under ## Decisions, NEVER Open questions). done/dismissed threads are TERMINAL + KEPT — never delete them.` +
    (errors.length ? `  ⚠ VALIDATION ERRORS (fix now): ${errors.join('; ')}` : '');

  const modeLine =
    mode === 'on'
      ? "AUTONOMOUS MODE = ON (the maintainer is away). What this MEANS: MAKE REASONABLE DECISIONS WITHOUT A HUMAN IN THE LOOP — do NOT ask questions, do NOT stall for confirmation; bias HARD to action and keep the background fleet busy. At a fork, pick the sensible option, DOCUMENT the call in the tracker, and PROCEED (choosing intelligently and letting the maintainer adjust on review beats stopping). Reconcile every completed sub-agent and immediately dispatch the next work. The ONLY things you may NOT autonomously land: a default / security-posture / product / brand / API-config-env decision the maintainer owns (recommend-only — design+prototype, surface to the tracker's decisions queue, don't flip the default), anything irreversible/destructive/published-external, and parked work he hasn't greenlit. Everything else: decide and do. Scan the board via `node scripts/fray/index.mjs`. You ARE empowered — CONTINUOUSLY cut patch releases (0.0.x: make version → commit → tag → push), push main, create repos, install tooling (brew), set up VMs, land greenlit work — yourself, no asking. Do NOT build an 'awaiting-the maintainer' queue from REVERSIBLE decisions (the maintainer's #1 repeated correction). The only true gates: a 0.1.0 bump, a truly-irreversible-destructive act, a user-facing DEFAULT (ship behind a flag, don't freeze), and public marketing wording."
      : `autonomous_mode=${mode} → interactive: surface decisions + ask rather than auto-landing.`;

  emit(`${core}  ${modeLine}  ${status}`);
} catch {
  emit(core);
}
