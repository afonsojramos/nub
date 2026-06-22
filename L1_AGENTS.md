# L1_AGENTS.md — operating manual for a DISPATCHED agent

You are reading this because a dispatch prompt told you to. It governs **how you operate as a dispatched agent** (an "L1" spawned by the main orchestrator "L0", or any agent spawned by another agent). It is distinct from [`AGENTS.md`](AGENTS.md), which is auto-loaded for L0 and carries project/repo context. Sub-agents do **not** auto-load `AGENTS.md` or skills — so the rules you need are gathered here, and every dispatch references this file by path.

These rules are hard-won (several were root-caused live). Follow them exactly.

## 1. NEVER pass a `name` field when you dispatch a sub-agent

Plain `Agent` tool + `run_in_background: true`, and **no `name`**. A `name` opts the child into the experimental **agent-teams** mode: completions route to the *team lead* (the main session), not to you, so you will **strand** waiting for a wake that never comes (and you get idle-ping spam). Nameless background dispatch is the simple sub-agent model where the child surfaces back to its spawner.

## 2. Nested fan-out is FRAGILE here — collect actively, never "stand by"

The hard mechanics (confirmed):
- A **rested sub-agent is NOT auto-re-woken** when its background child completes. Only the top-level main session (L0) is reliably re-woken by the harness.
- **Foreground/blocking Agent calls are hook-blocked** (a foreground child would block your turn; a human interjection orphans it).

Therefore, if you dispatch sub-agents:
- Do **not** end your turn "standing by for findings" — you will stall. (This exact mistake stranded an L1 sandbox orchestrator.)
- **Actively collect within your turn**: dispatch, then read each child's output file / poll until done, then integrate — all before you finish.
- **Keep fan-out shallow.** Prefer pushing heavy or wide parallel fan-out back up to L0 (which *is* reliably re-woken) or recommend the Workflow tool. Deep nesting (L1→L2→…) is where coordination breaks.
- If you genuinely cannot collect a child's result, **say so explicitly in your report** — never silently drop it or mark work done.

## 3. Own your thread

If your dispatch is tied to a `.fray/<slug>.md` thread, **edit it in place** — update `## Status`, `## Decisions`, `## Steps`, `## Next step`, and record your own agentId in the frontmatter `agents:` list, **before you finish**. Edit in place (single-voice current truth; no changelog append). Do not make the orchestrator re-transcribe your return. If you wrote a findings sidecar for a parallel fan-out, note its path in the thread.

## 4. Surface, don't guess — the stop criterion

Operate autonomously **only until** something human-owned or genuinely ambiguous arises:
- a default / security-posture / product / brand / API-config-env decision,
- a fork between materially different approaches with real tradeoffs,
- an unexpected blocker.

At that point **come to rest and surface it** to the orchestrator (who surfaces to the human) — do not decide it, do not land it. Mechanical/clearly-a-bug work is yours to finish; posture/default/architecture calls are recommend-only.

## 5. The two-level nested-implementer pattern (for substantive new functionality)

When you are an implementer of a non-trivial change, self-organize review using sub-agents (subject to §1–§2):
- **Plan** → write the implementation plan.
- **Plan-review** → dispatch a sub-agent (nameless, background; collect actively per §2) to critique the plan; take a second pass on what's valid.
- **Implement** against the refined plan.
- **Self-review** → dispatch reviewer(s); for a *major* change, multiple with different lenses (correctness / security / a subsystem).
- **Critically incorporate** → you judge the reviews on merit and fold in only the valid ones. Do **not** blind-trust — a reviewer has narrower/possibly-stale context than you. Reviews are advice, not verdicts.

Depth scales with blast radius: trivial change → no nesting; major change → the full loop. Given §2's fragility, if collecting reviewers reliably is hard, do the review serially within your turn or recommend L0 run it.

## 6. Verify, and report faithfully

Ground every load-bearing claim in code / a command / a doc you actually read — never memory. Run what you change. Report what's true: if tests failed, say so; if a step was skipped, say so; if you couldn't verify, say so. An empty or progress-only final message is an incomplete handoff — finish or report the blocker.
