# Lockfile mutation differential (`tests/mutation/`)

The write-path counterpart to [`tests/conformance/`](../conformance/). Every conformance fixture is a STATIC install round-trip (stage → real-PM install → nub frozen-read, or nub install → real-PM frozen-accept). None of them exercised `nub add` / `nub remove` / `nub update` against a lockfile a real PM already wrote — and the write path is where mutation bugs live: a static install can pass while an `add` churns the untouched portion, de-dups a shared transitive differently than the PM, or over/under-prunes on `remove`. This harness closes that gap.

## The loop

For each `(fixture, mutation, pm)`:

```
1. stage the fixture into TWO parallel copies:  nub/  and  ref/
2. the REAL PM installs in BOTH    -> identical pre-mutation baseline lockfile
3. MUTATE:
     nub/   ->  nub <add|remove|update> <args>
     ref/   ->  <pm> <add|uninstall|update> <args>   (the equivalent real mutation)
4. assert (a) FROZEN-ACCEPT:  the real PM frozen-installs nub's mutated lockfile
                              and does NOT rewrite it (cmp byte-identity across
                              the frozen run -- a frozen install is a no-op on a
                              well-formed lockfile).
5. assert (b) SEMANTIC EQUIVALENCE:  nub's mutated lockfile and the real PM's
                              mutated lockfile describe the SAME resolved graph,
                              ignoring ordering/formatting.
```

We always compare **nub-`<pm>` vs real-`<pm>`** — never cross-PM. Package managers legitimately resolve and de-dup differently from one another, so each nub-format lockfile is judged only against ITS OWN reference PM.

`nub <verb>` auto-detects the lockfile format already on disk (pnpm-lock.yaml / package-lock.json / bun.lock) and mutates that same format in place — so the harness never has to tell nub which format to write.

**yarn is read-only in nub** (no write-path mutation), so it is skipped entirely — unlike the conformance harness, which has a yarn Direction-B leg via `nub pm use yarn`.

## The semantic differential — why not `cmp`

Direction (a) uses byte-`cmp` (a *frozen* install must not rewrite the lockfile — that check is exact and correct). Direction (b) can NOT use `cmp`: `add` ordering, formatting, and incidental key order legitimately differ run-to-run and across PM versions, so byte-identity would false-fail on a semantically-correct mutation. Instead we extract a **normalized, order-insensitive graph** from each lockfile and compare those.

### `extract-graph.mjs <project-dir> [--format pnpm|npm|bun]`

Emits, for one lockfile:

```json
{
  "format": "pnpm",
  "direct":   { "<name>": "<declared-spec>", ... },
  "resolved": { "<name>@<version>": <count>, ... }
}
```

- **`resolved`** — the MULTISET of every concrete `name@version` in the lockfile. This is the load-bearing signal: it captures the three things a mutation changes and the bug classes the suite hunts, independent of each PM's nesting LAYOUT:
  - **add** (M.1) — the new dep + its transitives appear in the set.
  - **dedup** (M.3) — whether a shared transitive collapses to one version or keeps two shows up as one-vs-two keys.
  - **prune** (M.5/M.6) — removed/kept transitives are present/absent.
- **`direct`** — the root importer's declared specifiers (name → range). Captures the manifest-side mutation: `add pkg@^1` must write `^1` verbatim; `remove` must drop the entry.

Per-PM extraction (grounded in the real on-disk shapes, not memory):

| PM | lockfile | direct deps | resolved set |
|---|---|---|---|
| pnpm | `pnpm-lock.yaml` | root importer `.` `dependencies`/`devDependencies`/`optionalDependencies` (`name: { specifier }`) | flat `packages:` keys `name@version`, peer-suffix `(...)` stripped |
| npm | `package-lock.json` (v3) | `packages[""]` dep buckets | every `packages["node_modules/.../name"]` entry's `.version` (skips `link` workspace symlinks); nesting path ignored — last segment is the name |
| bun | `bun.lock` (JSONC) | `workspaces[""]` dep buckets | each `packages` value tuple's `[0]` = `name@version`; map key (nesting path) ignored |

All three normalize to the SAME graph for the same dependency set — that cross-format agreement is what makes the differential tolerant of layout while still catching a real divergence.

### `compare-graphs.mjs <a.json> <b.json>`

Diffs two extracted graphs for equality (exit 0 = equal, 1 = divergence with a human-readable diff, 2 = error). Refuses to compare two different `format`s (a harness wiring bug). Checks the `direct` spec map exactly and the `resolved` multiset exactly.

## Running

```sh
cargo build -p nub-cli                      # or use target/release/nub
tests/mutation/run-mutations.sh target/release/nub
tests/mutation/run-mutations.sh target/release/nub m3-add-dedup   # one fixture
KEEP=1 tests/mutation/run-mutations.sh ...   # keep the sandbox for forensics
SKIP_BUN=1 tests/mutation/run-mutations.sh ...
```

Pinned PMs (keep in sync with the conformance harness + the CI workflow): **npm 11.13.0 · pnpm 10.15.1 · bun 1.3.14**. The harness runs in a hermetic `HOME`/`XDG` sandbox and points bun at a throwaway `BUN_INSTALL_CACHE_DIR` so the frozen-accept integrity check is honest on a cold cache (exactly what a clean CI box does).

## Known-red mutation bugs

`expected-failures.txt` (`<fixture> <pm> <reason>`) gates the write-path divergences the differential has caught. Same discipline as conformance: the list must SHRINK, and a listed scenario that starts passing fails the run (XPASS-STALE) so the green flip is recorded by deleting the entry in the same commit as the fix.

Current reds (1 root cause, 2 legs): **npm save-prefix.** `nub add <pkg>@<bare-version>` writes the literal resolved version to `package.json` (`"ms": "2.1.3"`) where `npm install` applies its `^` save-prefix (`"ms": "^2.1.3"`). nub mirrors pnpm's literal-preserve convention regardless of the active PM (pnpm/bun legs pass — real pnpm/bun also write bare for an explicit version). An EXPLICIT range is preserved correctly on all PMs; only the bare-version → save-prefix-default path on npm diverges. This is a per-PM convention call (recommend-only).

## Adding a case

1. `mkdir fixtures/<id>` with a `package.json` (the pre-mutation starting state; mark it `"private": true`).
2. Add a `mutation` file with one line: `add: <args>` | `remove: <args>` | `update: <args>` (the args passed to both `nub <verb>` and the equivalent real-PM mutation). `#`-comment lines are ignored.
3. Add `<id>` to `ALL_FIXTURES` in `run-mutations.sh`.
4. Run it. If nub diverges from a real PM, gate it in `expected-failures.txt` with a precise reason (and open a fix thread) rather than letting it fail silently.

Picking a fixture graph: choose packages with small, stable, well-known dependency trees so the resolved set is deterministic (the `is-odd` / `is-even` / `is-number` / `kind-of` cluster is ideal — `is-even@1.0.0` depends on `is-odd@^0.1.2`, distinct from `is-odd@3.0.1`, which is exactly the multiple-version dedup case M.3 needs).

## Expanding to the full §M set

The catalogue (`.fray/lockfile-roundtrip-test-suite.findings/fixture-surface-catalogue.md` §M) lists 14 mutation cases. This harness prototypes M.1 (add no-conflict), M.3 (add → dedup at a different version), M.5 (remove → prune orphans). The remaining cases are mechanical additions once a fixture + `mutation` line is written:

- **M.2** `add <pkg>@<range>` with explicit caret/tilde/exact — verifies declared-spec preservation (nub already passes this on all PMs; a fixture pins it as a regression guard).
- **M.4** add that introduces a nested peer/version CONFLICT — differential per-PM (they legitimately differ; each matches its own).
- **M.6** remove where the removed dep's transitive is STILL needed by another dep (don't-prune-shared, inverse of M.5).
- **M.7/M.8/M.9** `--save-optional` / `--save-dev` / peer add — bucket correctness.
- **M.10/M.11** `update <pkg>` / `update` (all) — bump-within-range.
- **M.12** add in a WORKSPACE child — touches only the target importer.
- **M.13** add an npm-alias dep. **M.14** un-dedup (force two versions where there was one).
