---
title: "start.md adoption flow: offer to add a GENERAL nub skill (not a migration skill)"
status: needs-decision
last_update: 2026-06-21
status_text: "PR #55 open (sha 9c7d3fc) — step 7 now shows the explicit pipe form `nub agent skill > .claude/skills/nub/SKILL.md`. Confirmed `nub agent skill` emits a complete SKILL.md with valid frontmatter (name/description). Colin reviews wording + placement before merge."
---

## Goal
Colin (2026-06-21): the `start.md` adoption flow (reworked into a permission-gated flow in #45) should ask the user whether they want a **general nub skill** added to their project (a Claude Code skill teaching agents the nub CLI surface). Explicitly NOT a migration skill — "what a dumb thing to add to a repo"; migration is covered by start.md itself + the /guides/bun-to-nub guide. So: a GENERAL nub skill offer, migration stays skill-free.

## Scope
1. Read the current `start.md` (reworked in #45 — find it under `site/`; it drives the permission-gated adoption flow). Understand its step structure.
2. Add a step that ASKS (permission-gated, like the rest of the flow) whether to add a general nub skill to the user's project (`.claude/skills/nub/` or wherever skills live), and if yes, writes it.
3. AUTHOR the general nub skill: concise, teaches an agent the nub CLI surface — `nub <file>` (TS-just-works runner), `nub run`, `nubx`, the pnpm-compatible PM (`install`/`add`/`remove`/`run`), `--node`/`NODE_COMPAT` compat mode, the incumbent behavior (reads existing PM config). Ground it in the real CLI surface (crates/nub-cli/src/cli.rs + the docs). Brand-clean, factual, no aspirational claims.
4. NOT a migration skill — do not create one; migration is start.md + the guide.

## Decisions
- General nub skill offer in start.md = greenlit-concept (Colin). Build as a REVIEWABLE prototype PR — the exact skill content + the offer wording are Colin's to approve before it's the live default (product/UX surface).
- No migration skill.

## Open questions
- Exact skill content + where the offer sits in the flow — Colin reviews the prototype.

## Steps / follow-up queue
- [x] Locate + read start.md (#45's reworked flow) — `site/public/start.md`.
- [x] Add the permission-gated "add a general nub skill?" step (new step 7, after step 6, before Notes).
- [x] Skill content is the existing `site/public/skill.md` (already comprehensive; what `nub agent skill` outputs). No new skill file authored — the offer wires to the existing one.
- [x] PR #55 opened, enqueued.
- [x] Refinement (sha 9c7d3fc): step 7 shows the explicit pipe form `mkdir -p .claude/skills/nub && nub agent skill > .claude/skills/nub/SKILL.md`, writing to the conventional `.claude/skills/<name>/SKILL.md` path. Verified in `crates/nub-cli/src/agent/mod.rs` that `nub agent skill` prints `site/public/skill.md` verbatim (`print!("{SKILL_MD}")`), and that file leads with valid `name`/`description` YAML frontmatter (a unit test asserts it). So the piped file is a complete, conventionally-placed SKILL.md needing no post-edit. Added a `curl https://nubjs.com/skill.md` fallback.

## Next step
Colin reviews PR #55 — the offer wording and step placement in start.md. Once approved/adjusted, merge. No further agent work needed unless wording changes are requested.
