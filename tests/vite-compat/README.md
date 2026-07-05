# Vite symlink-GVS compat matrix (#315)

Validates that a Vite-powered project installed by nub — whose deps live in the
machine-global virtual store (`~/.cache/nub/pm/virtual-store`, OUTSIDE the
project root) — runs its dev server without the `403 … is outside of Vite
serving allow list` error, and that the fix is **works-without-nub** (the project
runs correctly when its dev server is invoked with no nub in the process).

## The fix under test

Two units, both in `crates/nub-cli/src/pm_engine/vite_compat.rs`, default-on
(opt out with `NUB_VITE_COMPAT=0`), gated on `vite` being in the graph:

- **Unit A — `node_modules/.modules.yaml`** (all Vite versions). nub writes JSON
  `{"virtualStoreDir":"<abs store>"}`. Vite ≥ 8.1 reads it natively and allows
  the store.
- **Unit B — dist backport** (Vite < 8.1). nub force-materializes just the
  `vite` package project-local (CAS store untouched) and codegen-inserts Vite's
  own 8.1 `.modules.yaml` sniff at the `fs.allow`-default computation site
  (`let allowDirs = server.fs.allow;` for v6/v7; `[searchForWorkspaceRoot(root)]`
  for v5). The sniff is YAML-tolerant + PM-agnostic (reads whatever
  `virtualStoreDir` any tool wrote — never hardcodes nub's path).

Because the fix lives in `node_modules` on disk (`.modules.yaml` + the patched
vite dist) and nothing is injected at runtime, it works regardless of whether
nub is in the process.

## The version tiers this matrix must cover

Each framework pins its OWN Vite, so the matrix spans both code paths:

- **native ≥ 8.1** — `.modules.yaml` only, no patch. (create-vite@latest today.)
- **backport < 8.1** — the ejected-vite dist patch. (Frameworks that pin Vite
  5/6/7: many VitePress/Storybook/older-framework releases.)

`driver.sh` records the exact Vite version + which tier each case exercised, so
the run PROVES both paths fire on real frameworks — not just on raw `vite`.

## How to run

```sh
export NUB=~/.cache/nub/worktrees/vite-build-target/fast/nub   # or target/fast/nub
# one framework, already scaffolded into <dir>, dev binds :5180
tests/vite-compat/driver.sh <dir> "npx vite dev --port 5180 --host 127.0.0.1" "npx vite build" 5180
```

`scaffold.sh <case> <dir>` non-interactively scaffolds the common cases and
prints the `DEV=`/`BUILD=` commands to feed `driver.sh` (substitute the port).
Framework CLIs change their flags often; treat `scaffold.sh` as a starting point
and adjust per release.

`driver.sh` emits one `ROW|…` line per case:

```
ROW|<name>|vite=<ver>|<tier>|<eject>|modules.yaml=<present/absent>|patched=<n>|/@fs=<code>|log=<no-403/403-in-log>|build=<ok/FAIL>
```

Acceptance for a case: `/@fs=200`, `log=no-403`, `build=ok`, and — for `< 8.1` —
`patched>=1`; for `>= 8.1`, `modules.yaml=present` with `patched=0`.

## Frameworks in scope

The `dev` server is run via each project's OWN bin/CLI (no nub) = works-without-nub.

| Framework | Notes |
|---|---|
| Astro + `@astrojs/react` | The literal #315 case (also vue/svelte integrations). |
| SvelteKit | `sv create` skeleton. |
| create-vite | `react-ts`, `vue-ts`, `svelte-ts`, `solid`, `preact-ts`, `lit-ts`, `vanilla-ts`. Note which 403 vs which pre-bundle into `.vite/deps` and never 403 (a bare create-vite app pre-bundles its deps project-local, so it does not 403 unless a dep is served raw via `/@fs` — SSR externals, `optimizeDeps.exclude`, framework client entries). |
| VitePress | Docs SSG; often pins Vite 5/6 ⇒ backport path. |
| Storybook (Vite builder) | `@storybook/react-vite`; `storybook build`. |
| SolidStart / Qwik / Remix · React-Router v7 / Analog (Angular+Vite) / Marko / Ionic | Extend `scaffold.sh`; each pins its own Vite. |

## Fidelity

`driver.sh` asserts at the HTTP layer: it fetches the REAL store-resident module
the browser requests via `/@fs` and greps the dev-server log for the literal 403
string. A **chrome-devtools MCP** browser pass (navigate the dev URL, confirm the
island/route hydrates and is interactive, read the console for 403s) is a
stronger check and SHOULD be run for the flagship Astro+React case when that MCP
is available; the HTTP + log-scan floor here is the CI-portable substitute.

## The Nuxt note

`nuxt` is currently on nub's `disableGlobalVirtualStoreForPackages` trigger
(installs all-disk, project-local), so it does NOT run symlink-GVS and this fix
does not apply to it as shipped. Nuxt IS Vite-based, so it is a candidate to
REMOVE from the trigger once this fix lets it work under symlink-GVS — see the
thread's `impl.md` sidecar for the finding. Changing the trigger default is a
maintainer decision, not part of this PR.
