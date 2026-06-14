// UserPromptSubmit hook — injects a model-only orchestrator reminder each turn.
// Run with nub (dogfood): `nub .claude/hooks/iw-reminder.ts`.
//
// DYNAMIC: reads `autonomous_mode` from the epic tracker's YAML front-matter
// (the IW structured-state store) so autonomous-specific nudges fire only when
// actually in autonomous mode, and parses the checkbox counts to give a
// high-level board status. Emits hookSpecificOutput.additionalContext (model-
// only). Robust: any failure → just the always-applicable core reminder; never
// throws (a broken hook must not disrupt the prompt).
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

// Token-saving: skip entirely inside sub-agent contexts. The hook stdin carries
// `agent_id` ONLY when fired inside a sub-agent (UserPromptSubmit shouldn't fire there
// at all, so this is belt-and-suspenders). Main session → no agent_id → proceed.
try {
  const hi = JSON.parse(readFileSync(0, "utf8"));
  if (hi.agent_id ?? hi.agentId) process.exit(0);
} catch {
  /* no stdin / not JSON → assume main session, proceed */
}

const core =
  "⟦orchestrator reminder⟧ You are the ORCHESTRATOR: delegate ALL project work — code/doc edits, GitHub writes (comments/PR edits/resolves), builds, tests, investigations — to BACKGROUND sub-agents; never do them yourself in the foreground. Your foreground = dispatch, synthesize returns, decide, and edit your own control surfaces (the fray board/threads + memory/skill/settings) + final reviewed git. Keep the fray board + thread files (.fray/_board.md + .fray/<thread>.md) synced THIS turn: fold every returned sub-agent's facts into its thread (or the board for one-shots), advance statuses, surface decisions/questions; re-derive the board's Threads table with `node scripts/fray/index.mjs --write`. HYGIENE: a thread is a LIVING STATUS of CURRENT work, NOT a changelog — DELETE done items (never log them chronologically), keep structured state in the board's YAML front matter, never let it accrete. ONLY the orchestrator edits the board + thread files (sub-agents write findings sidecars, never the canonical docs). Reconcile EVERY in-flight sub-agent; never drop a thread. Before asserting how nub/aube is STRUCTURED, ground it in wiki/architecture.md / the nub-aube-architecture memory / code you just read — never reason from stale or secondhand framing.";

function emit(ctx: string): never {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: ctx },
    }),
  );
  process.exit(0);
}

try {
  const dir = process.env.CLAUDE_PROJECT_DIR ?? ".";
  const board = readFileSync(join(dir, ".fray/_board.md"), "utf8");

  const fm = board.match(/^---\n([\s\S]*?)\n---/);
  const mode = fm?.[1].match(/^autonomous_mode:\s*(\S+)/m)?.[1] ?? "unknown";

  // fray: thread pulse from each .fray/<thread>.md frontmatter status.
  const counts: Record<string, number> = {};
  try {
    for (const f of readdirSync(join(dir, ".fray"))) {
      if (!f.endsWith(".md") || f === "_board.md") continue;
      const st = readFileSync(join(dir, ".fray", f), "utf8").match(/^status:\s*(\S+)/m)?.[1] ?? "?";
      counts[st] = (counts[st] ?? 0) + 1;
    }
  } catch {
    /* no .fray dir yet → empty pulse */
  }
  const pulse = Object.entries(counts).map(([s, c]) => `${c} ${s}`).join(" · ") || "none";
  const status = `threads (.fray): ${pulse}.`;

  const modeLine =
    mode === "on"
      ? "AUTONOMOUS MODE = ON (the maintainer is away). What this MEANS: MAKE REASONABLE DECISIONS WITHOUT A HUMAN IN THE LOOP — do NOT ask questions, do NOT stall for confirmation; bias HARD to action and keep the background fleet busy. At a fork, pick the sensible option, DOCUMENT the call in the tracker, and PROCEED (choosing intelligently and letting the maintainer adjust on review beats stopping). Reconcile every completed sub-agent and immediately dispatch the next work. The ONLY things you may NOT autonomously land: a default / security-posture / product / brand / API-config-env decision the maintainer owns (recommend-only — design+prototype, surface to the tracker's decisions queue, don't flip the default), anything irreversible/destructive/published-external, and parked work he hasn't greenlit. Everything else: decide and do. READ the '## Empowerment' + '## Durable rules' sections of .fray/_board.md (the fray single source of truth — parse cheaply via `node scripts/md-toc/index.mjs .fray/_board.md` + `node scripts/todo/index.mjs .fray/_board.md --not-done|--section`; thread detail via `node scripts/fray/index.mjs`): you ARE empowered — CONTINUOUSLY cut patch releases (0.0.x: make version → commit → tag → push), push main, create repos, install tooling (brew), set up VMs, land greenlit work — yourself, no asking. Do NOT build an 'awaiting-the maintainer' queue from REVERSIBLE decisions (the maintainer's #1 repeated correction). The only true gates: a 0.1.0 bump, a truly-irreversible-destructive act, a user-facing DEFAULT (ship behind a flag, don't freeze), and public marketing wording."
      : `autonomous_mode=${mode} → interactive: surface decisions + ask rather than auto-landing.`;

  emit(`${core}  ${modeLine}  ${status}`);
} catch {
  emit(core);
}
