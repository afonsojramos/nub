# Workspace `--filter` / `-r` differential suite (vs real pnpm)

This directory is a rigorous differential test of nub's workspace selection — `--filter`'s full grammar and `-r`/`--recursive` — against **real pnpm** as the ground-truth oracle. It exists because a `--filter` ellipsis-direction bug nearly shipped (leading-vs-trailing `...` was inverted) while in-crate coverage was thin: a selection or direction bug in `crates/nub-core/src/workspace/filter.rs` could silently change which packages a command touches. This suite makes that class of bug impossible to ship unseen — every selector form is run against both engines on the same workspace and the selected package sets are asserted equal.

It complements the single in-crate regression (`ellipsis_direction_matches_pnpm` in `filter.rs`) with a broad external matrix across several workspace topologies. The in-crate tests check nub against nub's own model; this checks nub against pnpm's actual behavior.

## The loop

1. `node make-fixtures.mjs [dest]` builds the fixtures (default `/tmp/nub-wsfilter-fixtures`) — one real workspace per topology. Each workspace carries **both** a `package.json#workspaces` (nub's native member source) and a `pnpm-workspace.yaml` with identical globs (pnpm's), so the two engines discover the *same* member set from their own native config. (nub only reads `pnpm-workspace.yaml` when pnpm is the incumbent PM — the brand hard gate — so the fixture gives nub `workspaces` directly rather than relying on that path.) Every member has an identical `whoami` script that prints `NUBPKG:<name>`.
2. `node run-filter-matrix.mjs [nub-binary] [fixtures-dir]` regenerates the fixtures, then for each `(topology × selector)` runs **real pnpm** and **nub** with the identical selector and asserts the two selected sets are equal. Defaults: nub = `target/release/nub` (then `target/debug/nub`); fixtures = `/tmp/nub-wsfilter-fixtures`.

No `install` is run — the membership oracle never needs `node_modules` materialized; both engines compute the selection from the manifests alone. The suite is therefore fast and fully offline.

### The membership oracle

`<tool> <selector> -r run whoami`. Every member runs the same script, which prints `NUBPKG:<its-name>`. The set of names that print is exactly the set of selected packages — and **both pnpm and nub emit those lines**, so the parse is symmetric and directly comparable. `-r` is passed on both invocations so the workspace member set engages (pnpm requires it; it is a no-op-safe addition for nub). pnpm's "no projects matched the filters" prints nothing and exits 0, which the oracle reads as the empty set — the correct ground truth for a selector that matches nothing.

## Topologies × selectors

Six topologies, each chosen to make a distinct selector behavior observable:

| Topology | Shape | What it exercises |
| --- | --- | --- |
| `linear` | a→b→c→d | ellipsis DIRECTION (the core case): `pkg...` vs `...pkg`, `^` exclude-self, exclude, union |
| `diamond` | a→{b,c}→d | shared transitive dep; dependents/dependencies fan-out and re-convergence |
| `wide-fan` | hub→leaf1..6 | name globs (`leaf*`), one-to-many dependents, expand-then-subtract |
| `islands` | x1→x2 \| y1→y2 | selection containment — a filter on one island must never reach the other |
| `nested-dirs` | `packages/*` + `apps/*` | directory selectors: `./apps`, `./apps/*`, `{packages}`, exact dir, `...name` across dirs |
| `dev-prod-mix` | app→lib (prod), app→tool (dev) | prod vs dev dependency edges in the dependency/dependent walk |

The selector matrix (in `run-filter-matrix.mjs`, `matrixFor`) covers the full pnpm `--filter` grammar: exact name, name glob (`leaf*`), dir parent (`./apps`), dir glob (`./apps/*`), `{dir}`, exact dir, `pkg...` (deps), `...pkg` (dependents), `...pkg...` (both), `pkg^...` / `...^pkg` (exclude-self), `!pkg` (exclude), repeated `--filter` (union), `--filter` + `!` (subtract), `-r` alone, and `-r` combined with `--filter`. It is **comprehensive, not exhaustive**: each grammar form is asserted where a topology actually distinguishes it, not re-run identically across topologies that cannot tell two selectors apart.

### Gaps (deliberate)

- **`[since]` git-ref selectors** (`--filter '[origin/main]'`) are not covered: they require a git repo with commits and a diff, which would make the fixtures stateful and slow. nub's git-ref parsing/diff path (`packages_changed_since`) is exercised by in-crate unit tests instead. This is the one selector family the differential matrix skips; note it if extending.

## Running

```sh
# from the repo root, with a release build present:
node tests/workspace-filter/run-filter-matrix.mjs

# against a debug build / explicit binary / explicit fixtures dir:
node tests/workspace-filter/run-filter-matrix.mjs target/debug/nub /tmp/wsf
```

Exit code is **0** when every selector matches pnpm (known divergences are XFAIL and do not fail the run) and **non-zero** when a NEW divergence appears, with the topology, selector, and both sets printed so the failure is debuggable from CI logs alone. If `pnpm` is not on `PATH` the suite **skips** (exit 0, clear message) rather than failing — so it slots into CI on any leg, gated by pnpm availability.

### Tier model

Workspace selection is pure manifest analysis in `nub-core` — it does **not** branch on the Node fast-tier vs compat-tier split that PnP/transpile behavior does, and it does not depend on the Node version. So unlike `tests/pnp/`, this suite does **not** need a Node-version sweep: one run on any supported Node is representative. (The membership oracle does spawn Node to print the sentinel, but the *selection* under test is computed before any script runs.) The only environmental requirement is a real `pnpm` on `PATH`. CI should run this on one leg with pnpm installed.

## Known divergences

None. Triaged divergences are listed in `KNOWN_DIVERGENCES` in `run-filter-matrix.mjs` so the suite stays green on them (tracked, not a regression) while still failing on anything new; the set is currently empty. Remove the corresponding `KNOWN_DIVERGENCES` entry once nub is fixed, so the matrix re-asserts parity.

### D1 (FIXED 2026-06-15) — bare directory selector: exact dir only, no recursion

**pnpm:** `--filter ./dir` and `--filter {dir}` select a package **only when `dir` itself is that package's directory** (holds its `package.json`). They do **not** select packages nested *under* `dir` — that needs a glob (`./dir/*`, `{dir/*}`). This is pnpm's default glob dir-filtering (`useGlobDirFiltering` / `matchProjectsByGlob`): a literal selector glob-matches the workspace-relative dir, and a literal has no `**`, so it never reaches a nested child.

**The fix:** `matches_pattern` (`crates/nub-core/src/workspace/filter.rs`, the `strip_prefix("./")` branch) previously treated a bare dir as a recursive parent (`rel_dir == p || rel_dir.starts_with("p/")`). It now matches only the exact package dir (`rel_dir == p`); recursion is opt-in via an explicit `*`/`**` glob, handled by the glob branch above it. The matrix re-asserts parity:

| selector | pnpm = nub |
| --- | --- |
| `--filter ./apps` (parent, not a package) | `{}` |
| `--filter {packages}` (parent, not a package) | `{}` |
| `--filter ./apps/*` (glob) | `{api, web}` |
| `--filter ./packages/group` (dir IS a package, also a parent) | `{group}` |
| `--filter ./packages/group/*` (glob) | `{groupchild}` |

The sharper "dir is both a package and a parent" case is covered by the `dir-is-package-and-parent` topology, and the in-crate tests `dir_selector_matches_workspace_relative_path` + `bare_dir_selector_matches_package_not_its_nested_child` pin the rule.
