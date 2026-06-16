#!/usr/bin/env node
// @ts-check
// SessionStart hook — SEEDS the session's orchestrator context. Run directly with node
// (no transpiler — max Node compat; fray's own hooks never depend on nub to run):
// `node .claude/hooks/session-seed.mjs`.
//
// Fires on EVERY session start (startup/resume/clear/compact — enumerated in
// settings.json). It injects two layers:
//   1. `core` — the static orchestrator role + hygiene doctrine, on EVERY session start.
//      This used to be re-injected per-message by iw-reminder (UserPromptSubmit); it is
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

// The static orchestrator role + hygiene doctrine. Lifted VERBATIM from iw-reminder.mjs
// (the former authoritative copy) — it does not change within a session, so it seeds ONCE
// here instead of being re-injected on every prompt.
const core =
  '⟦orchestrator reminder⟧ You are the ORCHESTRATOR: delegate ALL project work — code/doc edits, GitHub writes (comments/PR edits/resolves), builds, tests, investigations — to BACKGROUND sub-agents; never do them yourself in the foreground. Your foreground = dispatch, synthesize returns, decide, and edit your own control surfaces (the fray board/threads + memory/skill/settings) + final reviewed git. Keep the fray threads (.fray/<thread>.md; globals in .fray/config.yml) synced THIS turn: fold every returned sub-agent\'s facts into its thread, advance its status, surface decisions/questions; scan the board on demand (`node scripts/fray/index.mjs`). HYGIENE: keep each thread\'s ## Status + ## Next current so the LIVE state isn\'t buried — but a thread CAN hold a full record (a done/dismissed thread SHOULD have a complete investigation write-up; do NOT wipe detail to keep it lean). Global structured state lives in config.yml. DONE/DISMISSED threads are KEPT, NEVER deleted — each is its own file, excluded from the active board + the pending list by status, so a finished thread is zero bloat (a core benefit of per-file threads; do NOT clean them up). ONLY the orchestrator edits the board + thread files (sub-agents write findings sidecars, never the canonical docs). Reconcile EVERY in-flight sub-agent; never drop a thread. NO IN-FLIGHT STEERING: you cannot message a running sub-agent in this harness — front-load everything into the dispatch prompt, and for a follow-up touching a file an agent already owns, mark it `enqueued` and dispatch the instant that agent returns rather than spawning a clobbering sibling or killing-and-respawning (killing orphans WIP). Before asserting how nub/aube is STRUCTURED, ground it in wiki/architecture.md / the nub-aube-architecture memory / code you just read — never reason from stale or secondhand framing.';

// The deep nub<->aube architecture model — injected ADDITIONALLY only after a compaction.
// Why this exists: after a long compacted session the orchestrator once asserted a
// badly-wrong nub<->aube model (treated aube's whole CLI surface as nub's), reasoning from
// stale post-compaction framing instead of the code. This re-injects the distilled model.
const grounding = `⟦nub architecture re-grounding (post-compaction)⟧ Context was just compacted. Re-seed the load-bearing nub<->aube model NOW, and re-read wiki/architecture.md + the cited code (and the nub-aube-architecture memory) before asserting ANY structural claim.

- WRAPPING = library embed, NOT CLI passthrough. nub has its OWN CLI (own clap, own verb registry crates/nub-cli/src/pm_engine/mod.rs::ENGINE_VERBS, dispatch crates/nub-cli/src/cli.rs) and calls aube::commands::<verb>::run(typed_opts) in-process as a linked Rust library (aube = path dep, vendored at vendor/aube). No subprocess, no aube CLI. aube's own cli_main + tool-identity subcommands (doctor/sponsors/completion/diag/usage) are DEAD under nub - its CLI/subcommand surface is irrelevant; only the engine run() fns nub calls matter.
- REBRAND: ALL engine output flows through crates/nub-cli/src/pm_engine/present.rs (ERR_AUBE_*->ERR_NUB_*, aube->nub spellings, jdx URLs stripped, exit-code map).
- INSTALL PIPELINE: CAS store ($XDG_DATA/aube/store/v1/files/, BLAKE3 content-addressed) + GVS global virtual store (~/.cache/aube/virtual-store/, on by default OUTSIDE CI). Materialization = per-file reflink->hardlink->copy (aube-linker/materialize.rs; reflink = APFS clonefile / btrfs FICLONE). Graph wiring = symlinks only.
- THREE INSTALL STATES, do NOT conflate: (1) reinstall-IN-PLACE (node_modules present + state-hash match) -> try_install_fast_path short-circuits, ZERO file ops ("Already up to date"). (2) WARM reinstall = warm store + lockfile but node_modules WIPED -> full OFFLINE materialize from local store -> THIS is the headline warm benchmark vs pnpm, and whole-dir clonefile is the lever on it. (3) COLD = nothing cached -> network + minimumReleaseAge cooling -> the ~7MB primer (top-package packument metadata) helps here only.
- wiki/architecture.md is load-bearing BUT its ~line 250 "no own package manager" / toolchain lines are STALE (contradicted by the vendored aube PM). Trust code + the nub-aube-architecture memory over those lines.
- EMPOWERMENT + FRAY: the control surface is **fray** — independent per-thread files .fray/<slug>.md + globals in .fray/config.yml (autonomous_mode + state). There is NO stored board; COMPUTE it on demand with \`node scripts/fray/index.mjs\`. Read the fray skill at .claude/skills/fray/SKILL.md (canonical thread structure: Goal · Status · Decisions · Open questions · Steps/follow-up queue · Next step; done/dismissed = terminal + KEPT). ONLY the orchestrator edits threads (sub-agents write .fray/<thread>.findings/<id>.md sidecars). You are empowered (continuously cut patch releases, push main, create repos, install tooling, land greenlit work); reversible action > freezing; do NOT build an "awaiting-the maintainer" queue from reversible decisions (the maintainer's #1 repeated correction).`;

// `core` on EVERY session start; `grounding` ADDITIONALLY only after a compaction.
const parts = [core];
if (input.source === 'compact') parts.push(grounding);

process.stdout.write(
  JSON.stringify({
    hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: parts.join('\n\n') },
  }),
);
process.exit(0);
