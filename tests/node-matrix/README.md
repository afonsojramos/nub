# Node-version matrix smoke

Augmentation smoke across a big Node-version matrix. nub augments the user's installed Node
through version-banded mechanisms that break **differently** on different Node versions:

- the **fast tier** (Node >= 22.15): sync resolve/load hooks via `module.registerHooks`;
- the **compat tier** (18.19 - 22.14): an async `module.register` loader worker;
- V8-flag and experimental-flag injection, gated per version.

The repo's main CI matrix exercises only a few Node versions and never set up the
**async-loader x sync-hooks collision** that shipped the Turbopack/Tailwind `resolveSync()`
crash (the `nextjs-build-compat` thread / PR #98). This matrix closes that gap.

## What it runs

`run.sh <nub-binary> [--collision-must-pass]` — the lightweight per-version smoke:

- **functional smoke** (every version): `hello.js`, `hello.ts` (TS transpile + a non-erasable
  enum), ESM-imports-CJS, `import.meta.resolve`, Worker threads.
- **async-loader collision** (`fixtures/async-loader-collision/`): the faithful, **bundler-free**
  reproduction of the resolveSync class. User code calls `module.register(<async loader>)`
  while nub's fast-tier sync `registerHooks` resolve hook is active; resolving the loader
  module's own specifier through the sync chain hits Node's `Hooks.resolveSync()` stub, which
  throws `ERR_METHOD_NOT_IMPLEMENTED` on Node-broken versions. (Reduced from `@tailwindcss/node`;
  verified to reproduce identically to the real Turbopack build through the actual nub binary.)

`real-apps.sh <nub-binary> [--require-pass]` — the heavy nightly real-app builds:

- **Next 16 + Tailwind v4 + Turbopack** — the in-the-wild manifestation of the crash.
- **Vite SSR** (`vite build --ssr` + executing the SSR bundle) — a small, dependency-light
  second real toolchain. Chosen over a dev-server/express scaffold for CI reproducibility.

## The Node-version bands (resolveSync crash)

Empirically bisected (see `.fray/nextjs-build-compat.findings/resolvesync-version-band.md`):

| Band | resolveSync stub present? |
|---|---|
| 18.19 - 22.14 (compat tier, no `registerHooks`) | no bug — different code path |
| 22.15 - 22.16+ (v22 LTS) | **BROKEN** (not backported) |
| 23.6 - 23.11 | **BROKEN** |
| 24.1 - 24.11.x | **BROKEN** |
| 24.12+ | fixed |
| 25.0 - 25.1 | **BROKEN** (regression) |
| 25.2+ | fixed |
| 26.x | fixed |

The collision guard is meaningful **only on a Node-broken version** — on a Node-fixed version
it is vacuously green. The matrix therefore runs it across the broken bands (and asserts it
must pass on the fixed ones).

## Important: nub must run the matrix-selected Node

nub discovers its Node from `PATH`, but an `engines.node` / `.node-version` pin **above** the
PATH version makes nub *provision* a different Node, silently masking a leg's coverage. The
fixtures ship a **pin-free `package.json`** so nub always augments the matrix-selected version,
and `run.sh` has a sanity guard that fails the leg if nub ran a different version than selected.
The repo root pins `engines.node >= 22.15`, which is why the fixtures need their own.

## XFAIL policy (read before flipping to hard-fail)

As of this matrix's introduction, **PR #98's fix does not actually recover the crash on the
Node-broken bands** (validated against the real binary — see
`.fray/node-version-matrix-smoke.findings/validation.md`):

1. #98's resolve-hook recovery is gated on `userAsyncLoaderActive()`, which is **false** at the
   throwing resolution (the loader module's own spec, resolved during `module.register`), so the
   recovery never fires.
2. Even bypassing that gate, the **load** hook hits the identical `loadSync` stub with no recovery.

So the collision guard currently **XFAILs** on Node-broken legs (it documents the known gap and
keeps the matrix green) and **must-pass** on Node-fixed / compat-tier legs. Once nub actually
recovers the sync-into-async hop on the broken bands, flip the broken legs to
`--collision-must-pass` (in `.github/workflows/node-matrix.yml`) so the matrix guards against a
regression of the fix.

## Reproduce locally

```sh
# Build the dev binary once (from the repo root):
cargo build -p nub-cli -p nub-native
mkdir -p runtime/addons && cp target/debug/libnub_native.so runtime/addons/nub-native.node

# Drive nub onto a specific Node via PATH (nub augments the PATH node):
PATH="$HOME/.nvm/versions/node/v24.11.0/bin:$PATH" tests/node-matrix/run.sh target/debug/nub
PATH="$HOME/.nvm/versions/node/v26.2.0/bin:$PATH"  tests/node-matrix/run.sh target/debug/nub --collision-must-pass

# Heavy real-app builds:
PATH="$HOME/.nvm/versions/node/v24.11.0/bin:$PATH" tests/node-matrix/real-apps.sh target/debug/nub
```
