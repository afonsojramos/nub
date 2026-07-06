# `tanstack-start` install-benchmark fixture

A real, currently-popular batteries-included Vite starter — [TanStack Start](https://tanstack.com/start) (React) — used to benchmark warm `install` across nub / pnpm / bun / npm on a GVS-eligible project (Vite is not on nub's global-virtual-store trigger list, so the store stays active, unlike the Next-based `t3` fixture).

## Provenance

Scaffolded from the official TanStack Start CLI (React framework, with example pages):

```
npx @tanstack/cli@latest create tanstack-start --framework React --yes --examples
```

Ships Vite 8, TanStack Router + Start (SSR), TanStack devtools, Tailwind 4, `@testing-library/react` + Vitest + jsdom, TypeScript. No cypress / playwright / electron / sharp — no binary-download postinstall, so the install is deterministic across all four package managers. Only `package.json` and the three lockfiles are committed — the benchmark installs dependencies, it does not build or run the app.

## Modifications (stability only — the dependency set is preserved)

- Pinned the deps the scaffold ships as `"latest"` (the `@tanstack/*` router/start/devtools packages) to the versions resolved at scaffold time (2026-07-06), so the fixture is stable across lockfile regenerations. The dependency set is unchanged.

## Lockfiles

Regenerate after editing `package.json`:

```
pnpm install --no-frozen-lockfile                  # pnpm-lock.yaml (nub + pnpm)
bun install --save-text-lockfile                   # bun.lock (bun)
npm install --package-lock-only --ignore-scripts   # package-lock.json (npm)
```

Resolved package counts: pnpm ~313 / npm ~316 / bun ~253.
