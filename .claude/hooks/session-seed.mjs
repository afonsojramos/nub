#!/usr/bin/env node
// @ts-check
// SessionStart hook — SEEDS the session's orchestrator context. Run directly with node
// (no transpiler — max Node compat; fray's own hooks never depend on nub to run):
// `node .claude/hooks/session-seed.mjs`.
//
// Fires on EVERY session start (startup/resume/clear/compact — enumerated in
// settings.json). It injects two layers:
//   1. `core` — the static orchestrator role + hygiene doctrine, on EVERY session start.
//      This used to be re-injected per-message by fray-reminder (UserPromptSubmit); it is
//      static within a session, so it belongs here (once at session start + once after each
//      compaction, exactly the cadence static doctrine wants) — NOT re-paid every turn.
//   2. `grounding` — the deep nub<->aube architecture model, ADDITIONALLY when
//      source==="compact". Compaction is the one event that drops the deep structural model.
//
// Why SessionStart and not PostCompact: PostCompact is OBSERVE-ONLY — its
// hookSpecificOutput.additionalContext is NOT delivered to the model (verified against the
// Claude Code hooks docs, 2026-06-14), so the old PostCompact wiring was a silent no-op.
// SessionStart additionalContext IS injected into the next turn. Robust: never throws (a
// broken hook must not disrupt the session).
import { readFileSync } from 'node:fs';
import { loadConfig } from '../../scripts/fray/config.mjs';

/** @type {{ agent_id?: unknown, agentId?: unknown, source?: string }} */
let input = {};
try {
  input = JSON.parse(readFileSync(0, 'utf8'));
} catch {
  /* no stdin / not JSON → input stays {} → proceed (fail-open to inject) */
}
// Skip inside sub-agent contexts (they carry agent_id).
if (input.agent_id ?? input.agentId) process.exit(0);
// fray kill-switch — `enabled: false` silences the hook (missing/unparseable → defaults → enabled, fail-safe).
if (loadConfig(process.env.CLAUDE_PROJECT_DIR ?? '.').enabled === false) process.exit(0);

// The static orchestrator role + hygiene doctrine. Lifted VERBATIM from fray-reminder.mjs
// (the former authoritative copy) — it does not change within a session, so it seeds ONCE
// here instead of being re-injected on every prompt.
const core =
  '⟦orchestrator reminder⟧ You are the ORCHESTRATOR: delegate ALL project work — code/doc edits, GitHub writes (comments/PR edits/resolves), builds, tests, investigations — to BACKGROUND sub-agents; never do them yourself in the foreground. Your foreground = dispatch, synthesize returns, decide, edit your own control surfaces (the fray board/threads + memory/skill/settings), and REVIEW+MERGE the PRs landing agents open. Substantive nub-repo work lands via a PR from an isolated git WORKTREE (agents open it, you merge); the SHARED main tree is NEVER branched/reset/stashed. Exceptions commit direct to main: control-surface edits, trivial docs, release version/tag commits. vendor/aube keeps the nub-fork workflow. Keep the fray threads (.fray/<thread>.md; globals in .fray/config.yml) synced THIS turn: fold every returned sub-agent\'s facts into its thread, advance its status, surface decisions/questions; scan the board on demand (`node scripts/fray/index.mjs`). HYGIENE: keep each thread\'s ## Status + ## Next current so the LIVE state isn\'t buried — but a thread CAN hold a full record (a done/dismissed thread SHOULD have a complete investigation write-up; do NOT wipe detail to keep it lean). Global structured state lives in config.yml. DONE/DISMISSED threads are KEPT, NEVER deleted — each is its own file, excluded from the active board + the pending list by status, so a finished thread is zero bloat (a core benefit of per-file threads; do NOT clean them up). WRITE-OWNERSHIP: the dispatched sub-agent EDITS ITS OWN thread .md directly (in-place ## Status/## Decisions/## Next step/## Steps + a findings sidecar for depth) — it has the full context and best represents its thread\'s truth. You do NOT re-transcribe what it wrote. The orchestrator\'s RESIDUAL thread role: cross-thread linkage + reversals, the human-decision queue, dispatch/synthesis across efforts, config.yml + the agents:[] dispatch binding (record each dispatched agentId in its thread frontmatter). Reconcile EVERY in-flight sub-agent; never drop a thread. ALWAYS STEER — DON\'T RE-DISPATCH (TOP PRIORITY; agent-teams is ON, SendMessage works on running AND completed sub-agents): to add context / redirect / fold scope into a file an agent owns, answer a question it raised, or RESUME an agent the instant a temporary blocker (a HOLD, a transient CI failure, a now-available input) clears, MESSAGE/RESUME that agent (by name or agentId) — never let it die and cold-redispatch a fresh replacement (loses its runbook + context), never spawn a clobbering sibling, never kill-and-respawn (orphans WIP). Front-load the prompt regardless; fall back to `enqueued` only when work genuinely needs another agent\'s completed output. Before asserting how nub/aube is STRUCTURED, ground it in wiki/architecture.md / the nub-aube-architecture memory / code you just read — never reason from stale or secondhand framing.';

// The deep nub<->aube architecture model — injected ADDITIONALLY only after a compaction.
// Why this exists: after a long compacted session the orchestrator once asserted a
// badly-wrong nub<->aube model (treated aube's whole CLI surface as nub's), reasoning from
// stale post-compaction framing instead of the code. This re-injects the distilled model.
const grounding = `⟦nub architecture re-grounding (post-compaction)⟧ Context was just compacted. Re-seed the load-bearing nub<->aube model NOW, and re-read wiki/architecture.md + the cited code (and the nub-aube-architecture memory) before asserting ANY structural claim.

- WRAPPING = library embed, NOT CLI passthrough. nub has its OWN CLI (own clap, own verb registry crates/nub-cli/src/pm_engine/mod.rs::ENGINE_VERBS, dispatch crates/nub-cli/src/cli.rs) and calls aube::commands::<verb>::run(typed_opts) in-process as a linked Rust library (aube = path dep, vendored at vendor/aube). No subprocess, no aube CLI. aube's own cli_main + tool-identity subcommands (doctor/sponsors/completion/diag/usage) are DEAD under nub - its CLI/subcommand surface is irrelevant; only the engine run() fns nub calls matter.
- REBRAND: ALL engine output flows through crates/nub-cli/src/pm_engine/present.rs (ERR_AUBE_*->ERR_NUB_*, aube->nub spellings, jdx URLs stripped, exit-code map).
- INSTALL PIPELINE: CAS store ($XDG_DATA/aube/store/v1/files/, BLAKE3 content-addressed) + GVS global virtual store (~/.cache/aube/virtual-store/, on by default OUTSIDE CI). Materialization = per-file reflink->hardlink->copy (aube-linker/materialize.rs; reflink = APFS clonefile / btrfs FICLONE). Graph wiring = symlinks only.
- THREE INSTALL STATES, do NOT conflate: (1) reinstall-IN-PLACE (node_modules present + state-hash match) -> try_install_fast_path short-circuits, ZERO file ops ("Already up to date"). (2) WARM reinstall = warm store + lockfile but node_modules WIPED -> full OFFLINE materialize from local store (the whole-dir clonefile path). (3) COLD = nothing cached -> network + minimumReleaseAge cooling -> the ~7MB primer (top-package packument metadata) helps here only.
- wiki/architecture.md is load-bearing BUT its ~line 250 "no own package manager" / toolchain lines are STALE (contradicted by the vendored aube PM). Trust code + the nub-aube-architecture memory over those lines.
- EMPOWERMENT + FRAY: the control surface is **fray** — independent per-thread files .fray/<slug>.md + globals in .fray/config.yml (autonomous_mode + state). There is NO stored board; COMPUTE it on demand with \`node scripts/fray/index.mjs\`. Read the fray skill at .claude/skills/fray/SKILL.md (canonical thread structure: Goal · Status · Decisions · Open questions · Steps/follow-up queue · Next step; done/dismissed = terminal + KEPT). The dispatched sub-agent EDITS ITS OWN thread .md in place (+ a .fray/<thread>.findings/<id>.md sidecar for depth); the orchestrator does cross-thread linkage/reversals, the decision queue, dispatch/synthesis, and config.yml + the agents:[] agentId binding — never re-transcribing what the agent wrote. You are empowered (continuously cut patch releases, merge agents' PRs, create repos, install tooling, land greenlit work — substantive work via PR from a worktree, exception-class edits direct to main); reversible action > freezing; do NOT build an "awaiting-maintainer" queue from reversible decisions (the #1 repeated correction).`;

// `core` on EVERY session start; `grounding` ADDITIONALLY only after a compaction.
const parts = [core];
if (input.source === 'compact') parts.push(grounding);

process.stdout.write(
  JSON.stringify({
    hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: parts.join('\n\n') },
  }),
);
process.exit(0);
