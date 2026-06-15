---
name: fray
description: "Use this skill when running a large, mixed set of Codex efforts - investigations, decided fixes, verifications, launch pushes, audits, or refactor campaigns - where the human wants to stay in the loop. Fray is an orchestrator-first Codex workflow: use the `.fray/` per-thread control surface, compute the board with `node scripts/fray/index.mjs`, reconcile returned sub-agents, preflight thread-scoped dispatches, and preserve the repo's thread, decision, and tracker constraints."
---

# Fray For Codex

Fray is the orchestrator-first workflow for large, mixed coding efforts. The main Codex session is the orchestrator and the only decider. Sub-agents are instruments: they gather facts, implement bounded fixes, verify, or review, then return findings for the orchestrator to fold into `.fray/`.

Use the repo's Fray control surface exactly as written: independent thread files under `.fray/`, a computed board, orchestrator-owned tracker edits, preflighted sub-agent dispatches, and explicit reconciliation of every return.

## Start Every Fray Turn

Run the pulse before meaningful work:

```bash
node .agents/plugins/fray-codex/scripts/codex-reminder.mjs
node scripts/fray/index.mjs
```

Use the output to reconcile returned agents, surface pending questions, and choose the next action. Do not use a separate native task list as the source of truth while Fray is active; `.fray/` is the todo substrate.

## Control Surface

- `.fray/<slug>.md`: one file per live multi-step or long-lived thread. The filename slug is the id.
- `.fray/config.yml`: globals only, including `enabled` and `autonomous_mode`.
- `.fray/<slug>.findings/<id>.md`: optional sidecars for durable sub-agent output.
- `node scripts/fray/index.mjs`: the board. There is no stored board.

Thread frontmatter must include `title` and `status`. Status vocabulary is:

`todo`, `active`, `blocked`, `needs-decision`, `done`, `dismissed`.

`done` and `dismissed` are terminal and kept. Never delete terminal threads just to reduce board noise.

Each thread body must keep these sections, in this order:

1. `## Goal`
2. `## Status`
3. `## Decisions`
4. `## Open questions`
5. `## Steps / follow-up queue`
6. `## Next step`

Edit thread bodies in place so they read as current truth. Do not accumulate a chronological changelog inside a thread.

## When To Create A Thread

Create `.fray/<slug>.md` first, before dispatching or doing substantial work, when an effort:

- will take two or more sub-agents,
- carries a human-owned decision,
- outlives the current turn,
- is split off from an existing effort, or
- is explicitly requested as a thread/spike.

One-shots do not need a file. Put loose cross-cutting one-offs in `.fray/backlog.md`.

Research and implementation stay in one thread. Retool the same file in place when research turns into build work.

## Write Ownership

Only the orchestrator edits `.fray/<slug>.md` and `.fray/config.yml`. Sub-agents must never edit canonical thread files. If a sub-agent needs to persist durable output, instruct it to write a sidecar under `.fray/<thread>.findings/<id>.md`, then fold the signal into the thread yourself.

## Dispatch Discipline

Before spawning a thread-scoped sub-agent, run the dispatch preflight. This is the Codex-side guardrail that validates the thread exists, adds the thread pointer, appends the standard follow-up epilogue, and records the ledger row.

For a thread-scoped sub-agent prompt:

```bash
printf '%s' "$PROMPT" | node .agents/plugins/fray-codex/scripts/codex-dispatch-preflight.mjs --thread <slug> --agent-type <explorer|worker|default>
```

The preflight:

- denies missing `.fray/<slug>.md` by exiting nonzero,
- ensures the prompt starts with `THREAD: <slug>`,
- appends the orchestration epilogue,
- writes `.fray/.dispatch-ledger.jsonl`.

Paste the emitted prompt into `multi_agent_v1.spawn_agent`. For a true one-shot with no thread, do not use `THREAD:`.
Use `--dry-run` only when testing the preflight itself; real dispatches should write the ledger.

## Sub-Agent Rules

Use sub-agents only when Codex policy allows it for the turn, and make each prompt self-contained. Include the relevant codebase map, exact task, constraints, allowed write scope, and expected final report.

For every coding worker prompt, include:

- work on `main`; no branches, stashes, worktrees, destructive git ops, or repo copies,
- other agents may be editing; do not revert unknown changes,
- edit only the assigned files/modules,
- commit small and often when appropriate for the repo,
- finish with changed paths, tests run, and `## Follow-ups`.

Sub-agents are not deciders. They may fix obvious bugs, but default/security/product/brand/API-config-env decisions route back to the human unless already greenlit. Opinionated implementation needs sign-off first; broad investigation does not.

Background sub-agents are headless. Do interactive/TTY probes in the orchestrator foreground.

## Reconcile Returns

At the top of every Fray turn, fold returned agents before answering new conversational threads:

- update `## Status`,
- move settled calls into `## Decisions`,
- keep only unresolved items in `## Open questions`,
- drain queued follow-ups in `## Steps / follow-up queue`,
- update `## Next step`,
- mark the ledger entry reconciled when you have folded it.

A returned agent is not handled until it is folded into the owning thread.

## Model And Effort Mapping

Use Codex model overrides sparingly. When a dispatch needs a tiering decision:

- `gpt-5.3-codex-spark`: scripted/mechanical harvests only.
- `gpt-5.4-mini`: cheap bounded probes or simple edits.
- `gpt-5.4`: supporting cast, test scaffolding, observable differential probes, nontrivial mechanical edits.
- `gpt-5.5`: default for landing code, architecture, subtle diagnosis, adversarial review, and load-bearing judgment.

Do not pass a higher tier just by habit; choose by judgment required. Re-verify cheap-tier load-bearing claims.

## Autonomous Mode

`autonomous_mode: on` means the human is away. Do not ask blocking questions. Make reversible decisions, document them in `## Decisions`, and keep dispatching. Park only genuinely irreversible, destructive, external, published, or human-owned default/security/product/brand/API-config-env calls.

When `autonomous_mode: off`, surface decisions and ask at checkpoints instead of auto-landing them.

## Validation

Use these checks:

```bash
node scripts/fray/index.mjs --validate
node .agents/plugins/fray-codex/scripts/codex-reminder.mjs --json
```

If you edit this skill, preserve the Fray invariants: `.fray/` is canonical, the board is computed, only the orchestrator edits thread files, thread-scoped dispatches are preflighted, and every returned agent is reconciled.
