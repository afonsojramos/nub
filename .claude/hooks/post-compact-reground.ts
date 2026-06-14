// PostCompact hook — re-seeds nub's load-bearing architecture model into the
// fresh post-compaction context, so structural knowledge is never silently lost
// across a compaction. Run via nub (dogfood): `nub .claude/hooks/post-compact-reground.ts`.
//
// Why this exists: after a long compacted session the orchestrator once asserted a
// badly-wrong nub<->aube model (treated aube's whole CLI surface as nub's), reasoning
// from stale post-compaction framing instead of the code. the maintainer asked for compaction
// DETECTION that re-grounds the architecture. This hook injects the distilled model as
// additionalContext (model-only) on every compaction. Robust: never throws.
import { readFileSync } from "node:fs";

// Token-saving: skip inside sub-agent contexts. Sub-agents auto-compact independently and
// don't need the orchestrator's architecture grounding; the hook stdin carries `agent_id`
// only inside a sub-agent. Main session → no agent_id → inject the grounding.
try {
  const hi = JSON.parse(readFileSync(0, "utf8"));
  if (hi.agent_id ?? hi.agentId) process.exit(0);
} catch {
  /* no stdin / not JSON → assume main session, proceed */
}

const grounding = `⟦nub architecture re-grounding (post-compaction)⟧ Context was just compacted. Re-seed the load-bearing nub<->aube model NOW, and re-read wiki/architecture.md + the cited code (and the nub-aube-architecture memory) before asserting ANY structural claim.

- WRAPPING = library embed, NOT CLI passthrough. nub has its OWN CLI (own clap, own verb registry crates/nub-cli/src/pm_engine/mod.rs::ENGINE_VERBS, dispatch crates/nub-cli/src/cli.rs) and calls aube::commands::<verb>::run(typed_opts) in-process as a linked Rust library (aube = path dep, vendored at vendor/aube). No subprocess, no aube CLI. aube's own cli_main + tool-identity subcommands (doctor/sponsors/completion/diag/usage) are DEAD under nub - its CLI/subcommand surface is irrelevant; only the engine run() fns nub calls matter.
- REBRAND: ALL engine output flows through crates/nub-cli/src/pm_engine/present.rs (ERR_AUBE_*->ERR_NUB_*, aube->nub spellings, jdx URLs stripped, exit-code map).
- INSTALL PIPELINE: CAS store ($XDG_DATA/aube/store/v1/files/, BLAKE3 content-addressed) + GVS global virtual store (~/.cache/aube/virtual-store/, on by default OUTSIDE CI). Materialization = per-file reflink->hardlink->copy (aube-linker/materialize.rs; reflink = APFS clonefile / btrfs FICLONE). Graph wiring = symlinks only.
- THREE INSTALL STATES, do NOT conflate: (1) reinstall-IN-PLACE (node_modules present + state-hash match) -> try_install_fast_path short-circuits, ZERO file ops ("Already up to date"). (2) WARM reinstall = warm store + lockfile but node_modules WIPED -> full OFFLINE materialize from local store -> THIS is the headline warm benchmark vs pnpm, and whole-dir clonefile is the lever on it. (3) COLD = nothing cached -> network + minimumReleaseAge cooling -> the ~9.9MB primer (top-package packument metadata) helps here only.
- wiki/architecture.md is load-bearing BUT its ~line 250 "no own package manager" / toolchain lines are STALE (contradicted by the vendored aube PM). Trust code + the nub-aube-architecture memory over those lines.
- EMPOWERMENT + FRAY: the control surface is now the **fray** board + thread files at .fray/ (migrated 2026-06-14 from epics/final-polish/todo.md). Read the '## Empowerment' + '## Durable rules' sections of .fray/_board.md (the fray single source of truth; parse via scripts/md-toc + scripts/todo; thread detail via 'node scripts/fray/index.mjs') — you are empowered (continuously cut patch releases, push main, create repos, install tooling, land greenlit work); reversible action > freezing; do NOT build an "awaiting-the maintainer" queue from reversible decisions (the maintainer's #1 repeated correction).`;

process.stdout.write(
  JSON.stringify({
    hookSpecificOutput: { hookEventName: "PostCompact", additionalContext: grounding },
  }),
);
process.exit(0);
