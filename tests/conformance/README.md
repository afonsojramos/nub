# Drop-in PM conformance harness

This harness proves nub is a true drop-in package manager in both directions. It exists because the only honest judge of lockfile compatibility is the real package manager that owns the format.

## The two directions

**Direction A — nub READS others:** the real PM writes its native lockfile, then `nub install --frozen-lockfile` must succeed without touching the lockfile and produce a correct `node_modules`.

**Direction B — others READ nub:** nub writes the lockfile, then the real PM must accept it with its frozen-install command (`pnpm install --frozen-lockfile` / `npm ci` / `bun install --frozen-lockfile`) without rewriting it (zero churn).

The yarn direction is asymmetric: yarn v1 lockfile *write* fidelity is unproven in the engine, so Direction B passes only if nub *refuses* to mutate a detected `yarn.lock` (non-zero exit + lockfile untouched). Direction A (yarn writes → nub reads) is the normal two-way check.

## Tool/version matrix (as of 2026-06-11)

All 16 legs PASS on this host:

| fixture | dir | pm | pm-version | result |
| --- | --- | --- | --- | --- |
| simple | A | npm | 11.13.0 | PASS |
| simple | A | pnpm | 10.15.1 | PASS |
| simple | A | yarn | 1.13.0 | PASS |
| simple | A | bun | 1.3.14 | PASS |
| simple | B | npm | 11.13.0 | PASS |
| simple | B | pnpm | 10.15.1 | PASS |
| simple | B | yarn | 1.13.0 | PASS |
| simple | B | bun | 1.3.14 | PASS |
| peers | A | npm | 11.13.0 | PASS |
| peers | A | pnpm | 10.15.1 | PASS |
| peers | A | yarn | 1.13.0 | PASS |
| peers | A | bun | 1.3.14 | PASS |
| peers | B | npm | 11.13.0 | PASS |
| peers | B | pnpm | 10.15.1 | PASS |
| peers | B | yarn | 1.13.0 | PASS |
| peers | B | bun | 1.3.14 | PASS |

Note: pnpm 11 is not yet available on this host (pnpm 10.15.1 is the current version). Add a pnpm 11 leg when the host is upgraded — pnpm 9→10 changed the lockfile format (`lockfileVersion: '9.0'`), so 10→11 should be confirmed.

## Fixtures

| fixture | what it exercises |
| --- | --- |
| `simple` | plain registry deps, direct + transitive overlap (`debug` → `ms`), a devDep |
| `peers` | peer dependencies — `react-dom@18` with a `peerDep` on `react@18`, exercising peer resolution and auto-install |

Both are small by design; the goal is a fast, signal-dense suite. The aube-conformance harness (`tests/aube-conformance/`) covers larger fixtures (workspaces, overrides, platform-conditionals, patched deps, git deps) for the Direction B side; this harness adds Direction A and is the regression guard for the bidirectional contract.

## How to run

```sh
tests/conformance/run.sh                          # uses target/release/nub, runs all fixtures
tests/conformance/run.sh target/debug/nub         # explicit binary
tests/conformance/run.sh target/release/nub peers # single fixture
KEEP=1 tests/conformance/run.sh                   # keep sandbox for forensics
SANDBOX_ROOT=/tmp/my-sandbox tests/conformance/run.sh  # reuse/inspect a specific sandbox
SKIP_YARN=1 tests/conformance/run.sh              # skip yarn legs
```

Requirements: network access to the npm registry (these are real installs), `node` on PATH, and each PM you want to exercise on PATH. Missing PMs print a `NOTE:` line and their legs are skipped — the suite does not fail on a missing tool.

The sandbox redirects `HOME` and all `XDG_*` dirs to a fresh temp root so no dev-box `.npmrc`, caches, or PM stores leak in and nothing leaks out. On failure the sandbox is kept and the per-leg log (`logs/<fixture>--<dir>--<pm>.log`) and staged project (`runs/<fixture>--<dir>--<pm>/`) are available for forensics.

## Relation to aube-conformance

`tests/aube-conformance/` is the comprehensive Direction B suite (nub writes → real PM reads), spanning eight fixtures and a `nub pm use` round-trip leg. This harness is complementary:

- **New in this harness:** Direction A (real PM writes → nub reads frozen). This direction was not tested anywhere before 2026-06-11.
- **Shared coverage:** Direction B with the `simple` and `peers` fixtures is redundant with what `aube-conformance` covers — both suites pass, and that's healthy (two independent witnesses for the same property).
- **Where to add harder cases:** complicated fixtures (workspaces, overrides, platform-conditionals, patched deps) belong in `tests/aube-conformance/fixtures/`; run both harnesses to cover both directions for those shapes.
