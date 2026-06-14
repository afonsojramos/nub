#!/usr/bin/env node
// @ts-check
// SessionStart hook (matcher: "compact") — re-seeds nub's load-bearing architecture
// model after a compaction, so structural knowledge is never silently lost. Run directly
// with node (no transpiler): `node .claude/hooks/post-compact-reground.mjs`.
//
// Why SessionStart and not PostCompact: PostCompact is OBSERVE-ONLY — its
// hookSpecificOutput.additionalContext is NOT delivered to the model (verified against the
// Claude Code hooks docs, 2026-06-14), so the old PostCompact wiring was a silent no-op.
// SessionStart with source=="compact" fires after a compaction AND its additionalContext
// IS injected into the next turn. Robust: never throws.
//
// Why this exists: after a long compacted session the orchestrator once asserted a
// badly-wrong nub<->aube model (treated aube's whole CLI surface as nub's), reasoning from
// stale post-compaction framing instead of the code. This hook re-injects the distilled model.
import { readFileSync } from 'node:fs';
import { loadConfig } from '../../scripts/fray/config.mjs';

/** @type {{ agent_id?: unknown, agentId?: unknown, source?: string }} */
let input = {};
try {
  input = JSON.parse(readFileSync(0, 'utf8'));
} catch {
  /* no stdin / not JSON → input stays {} → proceed (fail-open to inject) */
}
// Skip inside sub-agent contexts (they carry agent_id). And only reground on a COMPACTION
// SessionStart — not normal startup/resume/clear (the settings matcher already scopes to
// "compact"; this is belt-and-suspenders).
if (input.agent_id ?? input.agentId) process.exit(0);
if (input.source && input.source !== 'compact') process.exit(0);
// fray kill-switch — `enabled: false` silences the hook (missing/unparseable → defaults → enabled, fail-safe).
if (loadConfig(process.env.CLAUDE_PROJECT_DIR ?? '.').enabled === false) process.exit(0);

const grounding = `⟦nub architecture re-grounding (post-compaction)⟧ Context was just compacted. Re-seed the load-bearing nub<->aube model NOW, and re-read wiki/architecture.md + the cited code (and the nub-aube-architecture memory) before asserting ANY structural claim.

- WRAPPING = library embed, NOT CLI passthrough. nub has its OWN CLI (own clap, own verb registry crates/nub-cli/src/pm_engine/mod.rs::ENGINE_VERBS, dispatch crates/nub-cli/src/cli.rs) and calls aube::commands::<verb>::run(typed_opts) in-process as a linked Rust library (aube = path dep, vendored at vendor/aube). No subprocess, no aube CLI. aube's own cli_main + tool-identity subcommands (doctor/sponsors/completion/diag/usage) are DEAD under nub - its CLI/subcommand surface is irrelevant; only the engine run() fns nub calls matter.
- REBRAND: ALL engine output flows through crates/nub-cli/src/pm_engine/present.rs (ERR_AUBE_*->ERR_NUB_*, aube->nub spellings, jdx URLs stripped, exit-code map).
- INSTALL PIPELINE: CAS store ($XDG_DATA/aube/store/v1/files/, BLAKE3 content-addressed) + GVS global virtual store (~/.cache/aube/virtual-store/, on by default OUTSIDE CI). Materialization = per-file reflink->hardlink->copy (aube-linker/materialize.rs; reflink = APFS clonefile / btrfs FICLONE). Graph wiring = symlinks only.
- THREE INSTALL STATES, do NOT conflate: (1) reinstall-IN-PLACE (node_modules present + state-hash match) -> try_install_fast_path short-circuits, ZERO file ops ("Already up to date"). (2) WARM reinstall = warm store + lockfile but node_modules WIPED -> full OFFLINE materialize from local store -> THIS is the headline warm benchmark vs pnpm, and whole-dir clonefile is the lever on it. (3) COLD = nothing cached -> network + minimumReleaseAge cooling -> the ~7MB primer (top-package packument metadata) helps here only.
- wiki/architecture.md is load-bearing BUT its ~line 250 "no own package manager" / toolchain lines are STALE (contradicted by the vendored aube PM). Trust code + the nub-aube-architecture memory over those lines.
- EMPOWERMENT + FRAY: the control surface is **fray** — independent per-thread files .fray/<slug>.md + globals in .fray/config.yml (autonomous_mode + state). There is NO stored board; COMPUTE it on demand with \`node scripts/fray/index.mjs\`. Read the fray skill at .claude/skills/fray/SKILL.md (canonical thread structure: Goal · Status · Decisions · Open questions · Steps/follow-up queue · Next step; done/dismissed = terminal + KEPT). ONLY the orchestrator edits threads (sub-agents write .fray/<thread>.findings/<id>.md sidecars). You are empowered (continuously cut patch releases, push main, create repos, install tooling, land greenlit work); reversible action > freezing; do NOT build an "awaiting-the maintainer" queue from reversible decisions (the maintainer's #1 repeated correction).`;

process.stdout.write(
  JSON.stringify({
    hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: grounding },
  }),
);
process.exit(0);
