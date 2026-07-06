# `vitesse-lite` install-benchmark fixture

A real, community-recognized batteries-included Vite starter — the Vite-ecosystem counterpart to `create-t3-app` — used to benchmark warm `install` across nub / pnpm / bun / npm on a GVS-eligible project (Vite is not on nub's global-virtual-store trigger list, so the store stays active, unlike the Next-based `t3` fixture).

## Provenance

Scaffolded from [antfu/vitesse-lite](https://github.com/antfu/vitesse-lite) (Anthony Fu's opinionated Vite + Vue 3 starter — Vue Router, VueUse, UnoCSS, Vitest, `unplugin-auto-import`/`-vue-components`/`-vue-macros`, `@antfu/eslint-config`):

```
npx degit antfu/vitesse-lite
```

Only `package.json` and the three lockfiles are committed here — the benchmark installs dependencies, it does not build or run the app, so the starter's source tree is not needed.

## Modifications (portability + benchmark hygiene only — the dependency set is preserved)

The dependency set is exactly vitesse-lite's; the edits below only make it install identically under all four package managers and keep timing about linking, not side-effect scripts:

- Inlined the pnpm **catalog** version refs (`catalog:*`) to their literal versions from the starter's own catalog, and removed the `workspaces` object + `pnpm-workspace.yaml`. npm rejects the catalog / `workspaces`-object form; inlining is a faithful, version-preserving flatten.
- Removed the git-hooks dev tooling `simple-git-hooks` + `lint-staged` and the `postinstall` that runs them (not part of an install-timing benchmark; `simple-git-hooks`'s own postinstall also fails under nub's virtual store).
- Removed `@vue-macros/volar` (a dev-only Volar IDE type plugin whose `peerOptional` `vue-tsc@3.0.8` conflicts with the project's `vue-tsc@^3.2.5` and blocks strict npm).
- Dropped the `packageManager` corepack pin.

## Lockfiles

Regenerate after editing `package.json`:

```
pnpm install --no-frozen-lockfile                  # pnpm-lock.yaml (nub + pnpm)
bun install --save-text-lockfile                   # bun.lock (bun)
npm install --package-lock-only --ignore-scripts   # package-lock.json (npm)
```

Resolved package counts: pnpm ~685 / npm ~771 / bun ~579.
