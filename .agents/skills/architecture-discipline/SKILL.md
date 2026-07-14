---
name: architecture-discipline
description: >-
  Guide major system-design and architecture work toward the smallest complete,
  explicitly bounded design. Use for new subsystems; protocols; brokers,
  supervisors, or daemons; global coordination or state; sandbox and process
  lifecycle; broad refactors; threat-model decisions; or requests for the
  simplest or most elegant design. Do not use for routine bounded edits whose
  mechanism and blast radius are already settled.
---

# Architecture discipline

Make completeness a property of the approved boundary, not the amount of machinery. Satisfy the stated invariants, ordinary-user requirements, and threat model without silently weakening them; prefer an explicitly narrower scope over nominally comprehensive infrastructure that solves unapproved futures.

## Establish the boundary

Before choosing a mechanism, write down:

- Outcome and ordinary-user behavior.
- Approved correctness and security invariants.
- Threat model: protected assets, actors, capabilities, trust boundaries, and excluded threats.
- Non-goals and future slices.
- Actual scale and platform constraints.
- Acceptance criteria, proportionate validation, and a stop condition.

Keep future slices separate. If narrowing the boundary would change an explicit claim, pause for approval; never narrow it silently.

## Prove the requirement before choosing the mechanism

Reproduce the bug, invariant violation, or product need. State that evidence separately from every proposed fix: a real defect proves that a boundary needs protection, not that the first or most defensive mechanism is correct.

Compare at least:

1. The simplest platform, kernel, standard-library, or runtime primitive that meets the boundary.
2. Relevant established prior art at comparable scale.
3. Any custom mechanism, including the additional states, failure modes, operations, and portability burden it introduces.

Choose the smallest mechanism that completely satisfies the approved boundary. A narrower documented design can be better than a sprawling general one.

## Stop at the architecture gate

Obtain explicit human or recorded design approval before adding an unapproved:

- Subprocess supervisor, broker, daemon, or cross-process protocol.
- Global registry, coordinator, or shared mutable control plane.
- Invasive operating-system automation or persistent background lifecycle.
- Custom scheduler, overlapping state machine, or material blast-radius expansion.

Do not use implementation momentum as approval. If the approved design already names the mechanism and boundary, record that fact and continue without reopening the decision.

## Implement directly

Keep one clear owner and direct data flow. Prefer primitives already supplied by the platform over coordination infrastructure, and make every new state, task, channel, callback, registry, and cleanup path earn its place.

For Rust architecture or implementation, read [references/seasoned-rust.md](references/seasoned-rust.md) before settling the design or reviewing the diff.

## Triage review evidence

For each finding, record:

- Reproduced impact.
- Violated approved boundary or invariant.
- Relevance to the current slice and threat model.
- Simplest fix that restores the boundary.

Treat findings as evidence, not automatic implementation mandates. Fix demonstrated defects. Redesign when findings expose a bad mechanism. Explicitly defer or close harness-only, theoretical, or future-scope cases that do not cross the approved boundary.

## Run the simplification checkpoint

Stop and inspect the whole diff when production lines, files, types, state machines, or mechanisms expand materially beyond the approved plan. Ask:

- What would a seasoned maintainer delete, collapse, or express through ownership or platform primitives?
- Which mechanism exists only for a future slice, harness permutation, or theoretical interleaving?
- Can one owner, enum, direct call, or scoped resource replace overlapping control layers?
- Does the implementation remain proportional to the demonstrated requirement?

For Rust diffs, run the advisory detector:

```bash
node .agents/skills/architecture-discipline/scripts/rust-complexity-smells.mjs --base origin/main
```

Use `--diff-file <path>` for a saved diff and `--help` for transparent threshold controls. The detector reviews diff shape only: it does not infer authorship, prove a defect, or gate CI. Investigate each prompt against the actual boundary.

## Stop

Finish when the bounded acceptance criteria pass, security and behavior properties have proportionate tests, and a fresh review finds no unresolved boundary violation. Do not continue an open-ended hardening campaign toward zero theoretical risk.

Report the outcome, approved boundary, evidence, alternatives considered, chosen mechanism, any architecture gate, validation, deferred future slices, and the stop condition.
