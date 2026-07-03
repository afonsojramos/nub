# nub-phantom

An internal/eval tool that detects **undeclared (phantom) dependencies** of npm
packages, and scans the top-N most-downloaded packages to build the empirical
`packageExtensions`/force-materialize dataset from ecosystem data instead of
guesswork.

It is **not** part of the shipped `nub` binary — it is its own Cargo workspace,
`exclude`d from the root workspace (see the root `Cargo.toml`), because it depends
on the full oxc parser. Build it on demand:

```sh
cargo build --manifest-path crates/nub-phantom/Cargo.toml --release
```

## What a phantom is

A bare `import`/`require` in a package's **published, reachable** code (the graph
from `exports`/`main`/`bin`) that is NOT covered by any of: the package's own
`dependencies`/`optionalDependencies`/`peerDependencies` (including
`peerDependenciesMeta` optional peers), Node builtins, a self reference, or a
bundled dep.

The classifier is built to **not false-flag**:

- **Declared optional peers** (`peerDependenciesMeta.<x>.optional`) → their own
  category, never a phantom (the pick-your-plugin pattern).
- **Guarded loads** — inside `try`/`catch` or a conditional branch (`if`, `&&`,
  ternary) → classified **soft**, not a hard phantom.
- **Type-only imports** (`import type`, all-inline-`type`) → dropped (no runtime).
- **Unreached files** (tests/examples not referenced by the published surface) →
  never walked, so a `devDependencies`-only import there is not a phantom.
- **Non-packages** — URLs, framework virtuals (`$app`), template placeholders,
  other-runtime internals (`_http_common`) → dropped.

## Subpath-adapter class

A **hard phantom reachable only from a non-`.` `exports` subpath** (not the main
graph) is the *subpath-adapter* class: `<pkg>/<adapter>` statically imports a
consumer-installed backend it never declares (`@hookform/resolvers/zod` → `zod`).
This resolves on npm/yarn/pnpm (upward `node_modules` walk) but breaks under a
realpath-canonicalizing global virtual store. The scan reports its count as the
blast-radius metric.

## Usage

```sh
# Analyze specific packages (human-readable).
nub-phantom analyze @hookform/resolvers vue-router

# Scan the top-N most-downloaded packages; emit the categorized JSON report.
nub-phantom scan --top 5000 --concurrency 5 --json > scan.json

# Scan from a cached newline-delimited name list.
nub-phantom scan --from top5000.txt --json
```

The corpus for `--top N` is the `npm-high-impact` `topDownload` ranking, fetched
from the registry. Registry requests retry with backoff on HTTP 429.
