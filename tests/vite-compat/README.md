# Vite symlink-GVS compat matrix (#315)

Validates that a Vite-powered project installed by nub — whose deps live in the
machine-global virtual store (`~/.cache/nub/pm/virtual-store`, OUTSIDE the
project root) — runs its dev server without the `403 … is outside of Vite
serving allow list` error, and that the fix is **works-without-nub** (the project
runs correctly when its dev server is invoked with no nub in the process).

## The fix under test

Two units, both in `crates/nub-cli/src/pm_engine/vite_compat.rs`, unconditional
(core GVS correctness, no user opt-out), gated on `vite` being in the graph. The
pre-fix `403` break is reproducible against a built binary via the internal test
seam `__NUB_VITE_COMPAT_DISABLE=1` — an undocumented A/B control, not a user knob:

- **Unit A — `node_modules/.modules.yaml`** (all Vite versions). nub writes JSON
  `{"virtualStoreDir":"<abs store>"}`. Vite ≥ 8.1 reads it natively and allows
  the store.
- **Unit B — dist backport** (Vite < 8.1). nub disk-materializes just the
  `vite` package project-local (CAS store untouched) and codegen-inserts Vite's
  own 8.1 `.modules.yaml` sniff at the `fs.allow`-default computation site
  (`let allowDirs = server.fs.allow;` for v6/v7; `[searchForWorkspaceRoot(root)]`
  for v5). The sniff is YAML-tolerant + PM-agnostic (reads whatever
  `virtualStoreDir` any tool wrote — never hardcodes nub's path).

Unit B as shipped disk-materializes vite ONLY when it is a **direct** dep — a
raw `vite dev` app. A framework that embeds vite **transitively** (Astro 5 pins
`vite@^6`, `< 8.1`) loads its vite from a store-to-store sibling symlink, so the
direct-dep eject never reaches it and the store `/@fs` stays 403 (the #315
residual). Phantom-eject (unconditionally on for users) auto-detects an embedded
vite `< 8.1` and disk-materializes its `[framework … vite]` **ancestor closure**
(measured 5 packages for Astro 5, `~1.5%` of the tree — everything else stays
symlinked), so the framework loads a project-local vite that Unit B patches. The
pre-eject baseline (byte-for-byte the shipped Unit B) is reproducible via the
internal test seam `__NUB_PHANTOM_EJECT_DISABLE=1` — an undocumented A/B control,
not a user knob.

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

## The closure acceptance cases

The two cases the selective-subtree closure must satisfy. Eject is on by default,
so just run the driver:

```sh
tests/vite-compat/driver.sh <dir> "<dev-cmd>" "<build-cmd>" <port>
```

- **Astro 5 (rung 1 — the vite closure).** `astro@^5` pins `vite@6.4.3` (`< 8.1`),
  loaded library-embedded. The flag disk-materializes the `[astro, vite, …]`
  closure → the ejected vite gets Unit B's sniff → a bare `astro dev` (no nub in
  the process) serves a store-resident `/@fs` module `200` (was `403`).
  Acceptance: `/@fs=200`, `log=no-403`, `patched>=1`, and only the framework
  closure ejected (the rest of the tree stays symlinked).
- **Nuxt 4 (project-local, not symlink-GVS).** `nuxt@^4` embeds `vite@7.3.6`
  (`< 8.1`) AND pulls `vue-router@5`, which declares `@vue/compiler-sfc` an
  OPTIONAL PEER and unguardedly imports it from its `/vite` unplugin. That peer is
  the sticking point: the phantom scanner correctly treats an optional peer as NOT
  a hard phantom, so the rung-2 hoist never targets it (the `unstorage` →
  `@nuxt/devtools` HARD-phantom hoist DOES fire), and under symlink-GVS there is no
  store-root public-hoist `node_modules` to catch it — pnpm threads the peer down
  the Nuxt subtree and links it, nub's resolver does not in this graph, so `nuxt
  prepare` fails with `Cannot find package '@vue/compiler-sfc'`. So `nuxt` is on
  `disableGlobalVirtualStoreForPackages` and installs project-local, which builds
  the `.nub/node_modules/` hidden hoist tree (pnpm-parity public-hoist) that Node's
  upward walk reaches → the optional peer resolves. Acceptance: `nuxt prepare` →
  "Types generated", `nuxt dev` page `200` with SSR-rendered markup, `0` errors,
  bare `nuxt dev` (no nub in the process). The proper symlink-GVS fix is resolver
  optional-peer-threading parity (a separate, maintainer-owned effort because of
  its lockfile-churn blast radius).

## The Nuxt trigger note

`nuxt` is on nub's `disableGlobalVirtualStoreForPackages` trigger (installs
all-disk, project-local), so it does NOT run symlink-GVS — this is deliberate (see
the Nuxt case above): the optional-peer edge Nuxt pulls via `vue-router@5` is not
placeable under symlink-GVS, and the project-local hidden hoist tree resolves it.
Keeping Nuxt under symlink-GVS is unblocked only by resolver optional-peer-threading
parity, which is out of scope here.
