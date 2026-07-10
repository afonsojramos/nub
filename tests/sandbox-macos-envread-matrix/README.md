# sandbox macOS env-read version matrix (ad-hoc CI probe)

Branch-scoped probe (per the `ci-adhoc-test` skill) that runs the sandbox's env-read
closure suite across macOS majors to record, from **real runner output**, which macOS
versions the closure holds on — and thereby pick a supported macOS floor.

## What it validates

The env-read closure is the Seatbelt profile fragment
`(deny process-info*)` + `(allow process-info* (target self))` emitted by
`crates/nub-sandbox/src/backend/macos.rs`. It blocks a confined child from recovering a
**scrubbed** secret out of a co-resident same-uid process's environment via
`sysctl(KERN_PROCARGS2)` — the vector that would otherwise make env confinement
worthless (the withheld var is merely absent from the child's own environ; without the
closure the child just reads it back out of a sibling/parent).

The profile is **version-independent in code** but was only ever exercised on
`macos-latest`. "macOS 13/14 likely leaks" was asserted, never verified. This probe
verifies it.

## Suites run (per version)

- `cargo test -p nub-sandbox --test macos_envread` — the flagship. Each assertion is
  paired with a **negative control** (closure lifted → the read LEAKs), so a HOLD is
  never hollow: the neg-control proves the `KERN_PROCARGS2` vector is genuinely live on
  that runner, and the confined read proves the closure shuts it (EPERM/`BLOCKED`).
  Covers: sibling-env read, same-sandbox-child read (the `(target self)` vs
  `(target same-sandbox)` discriminator), self-read survival, and `node` running under
  the closure.
- `cargo test -p nub-sandbox --test macos_enforcement` — supporting signal: the general
  Seatbelt fs/net enforcement suite. If confinement is broken at all on an older major,
  it surfaces here too.

## Verdict per version

The workflow classifies each runner from the suite's own assertion messages:

- **HOLD** — confined cross-process env read denied (`BLOCKED`/EPERM); neg-control LEAKed. Closure holds.
- **LEAK** — confined read recovered the scrubbed secret. Closure FAILS on this version.
- **INFRA** — neg-control never LEAKed (the vector didn't go live on the runner). Inconclusive, not a pass.
- **OTHER** — a non-leak regression (self-read or `node` broke). Inspect the log.

Only HOLD passes the job; LEAK/INFRA/OTHER fail so an inconclusive run can't read green.
The full per-version output lands in the run's job summary.

## Run it

No PR required. Push to the `sandbox-macos-envread-matrix` branch (an empty commit
re-runs an unchanged harness):

```sh
git commit --allow-empty -m rerun && git push
gh run list --workflow sandbox-macos-envread-matrix.yml --branch sandbox-macos-envread-matrix
```

`workflow_dispatch` is inert until the file reaches the default branch (GitHub only
registers dispatch for workflows on `main`) — the `push` trigger is what fires it here.

## Results

Findings + the recommended minimum supported macOS version are recorded in
`wiki/research/sandbox-macos-version-matrix.md` (local-only wiki).
