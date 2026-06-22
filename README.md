# Nub

A fast all-in-one toolkit that *augments* Node.js instead of trying to replace it. It provides a Bun-like modern DX on top of stock `node`. Written in Rust.

```sh
nub index.ts             # TypeScript-first Node.js runtime
nub run dev              # 24× faster pnpm run
nubx prisma generate     # 19× faster npx
nub install              # 2.5× faster pnpm install
nub watch src/server.ts  # native watch mode
nub pm shim              # built-in Corepack-style shims
nub node install 22      # Node version manager
nub upgrade              # self update
```


It provides a Bun-like modern DX on top of stock `node`. One tool to run your files and scripts, install dependencies, and manage Node itself. No new runtime, no vendor-specific API surface, no lock-in.

| Nub | Instead of |
|---|---|
| `nub <file>` | `node`, `tsx`, `ts-node`, `dotenv-cli` |
| `nub run <script>` | `npm run`, `pnpm run` |
| `nubx` | `npx`, `pnpm dlx / exec` |
| `nub install` | `npm`, `pnpm` |
| `nub watch` | `nodemon`, `node --watch`, `tsx watch` |
| `nub node` | `nvm`, `fnm`, `n`, `volta` |
| `nub pm` | `corepack` |

**Documentation:** https://nubjs.com/docs

## Install

```sh
# macOS / Linux
curl -fsSL https://nubjs.com/install.sh | bash

# Windows (PowerShell)
irm https://nubjs.com/install.ps1 | iex

# Or via npm (pnpm / yarn global add work too)
npm install -g --ignore-scripts=false @nubjs/nub
```

For GitHub Actions, use [`nubjs/setup-nub`](https://github.com/nubjs/setup-nub) in place of `actions/setup-node`. It's one-to-one compatible.

```diff
- - uses: actions/setup-node@v4
+ - uses: nubjs/setup-nub@v0
```

## File runner — `nub <file>`

A flag-for-flag drop-in for `node <file>` that also runs TypeScript and JSX directly — no tsconfig, no build step. A `.ts` file starts on par with plain `node`:

```sh
nub index.ts             # TypeScript, JSX, no build step
nub --watch app.ts       # same path, restart-on-change
```

- 🦆 Full TypeScript support — non-erasable syntax (`enum`, `namespace`, parameter properties), `emitDecoratorMetadata` decorators
- ⚛️ JSX / TSX
- 🧭 TypeScript-friendly resolution — extensionless imports, remaps `.js → .ts`, `tsconfig.json#paths`
- 🆕 Modern syntax like `using` — transpiler-downleveled on earlier versions of Node
- 🔐 Automatic `.env*` loading — Next.js/Vite parity
- 🗂️ Built-in loaders for common data formats — `.yaml`, `.toml`, `.jsonc`, `.json5`, `.txt`
- 🌐 Polyfills for missing APIs — `Temporal`, `Worker`, `URLPattern`, `WebSocket`, `EventSource` (these are mostly supported natively on recent versions of Node.js)
- 🔥 Unflagged experimental features — `node:sqlite`, `vm.Module`, `localStorage`
- ⚡ 2.9× faster startup than `tsx`

### Watch mode

Restart-on-change driven by the resolved dependency graph plus the off-graph files that still invalidate a run — no glob list to maintain:

```sh
nub watch src/server.ts
nub --watch src/server.ts   # same path
```

- 👀 Tracks the resolved dependency graph automatically
- 🧷 Also watches the off-graph invalidators — `.env*`, the `tsconfig.json` extends chain, `package.json`
- ⚙️ Runs on Node's own `--watch` engine, preserving output by default

View the [full watch mode docs 👉](https://nubjs.com/docs/watch).

### Modern APIs

Modern globals — TC39, web-platform, and newer Node built-ins — work out of the box under Nub, native where Node ships them and polyfilled or unflagged where it doesn't. The **minimum version** column is the lowest Node where the API works under Nub; a dash means the full supported range, 18.19 and up.

| API | Minimum version | How |
|---|---|---|
| [`Temporal`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Temporal) | — | polyfill below Node 26, native above |
| [`URLPattern`](https://developer.mozilla.org/en-US/docs/Web/API/URLPattern) | — | polyfill below Node 24, native above |
| [`RegExp.escape`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/RegExp/escape) | — | polyfill below Node 24, native above |
| [`Error.isError`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Error/isError) | — | polyfill below Node 24, native above |
| [`Promise.try`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise/try) | — | polyfill below Node 24, native above |
| [`Float16Array`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Float16Array) | — | polyfill below Node 24, native above |
| [`navigator.locks`](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API) | — | polyfill below Node 24.5, native above |
| [`reportError`](https://developer.mozilla.org/en-US/docs/Web/API/Window/reportError) | — | polyfill |
| [`vm.Module`](https://nodejs.org/api/vm.html#class-vmmodule) | — | flag-injected |
| [`ShadowRealm`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/ShadowRealm) | — | flag-injected |
| [`Wasm module imports`](https://nodejs.org/api/esm.html#wasm-modules) | — | flag-injected below Node 24.5 (22.19 on the 22.x line), native above |
| [`WebSocket`](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) | Node 20.10 | flag-injected below Node 22, native above |
| [`EventSource`](https://developer.mozilla.org/en-US/docs/Web/API/EventSource) | Node 20.18 | flag-injected below the native line, native above |
| [`node:sqlite`](https://nodejs.org/api/sqlite.html) | Node 22.5 | flag-injected below Node 22.13, native above |
| [`addon imports`](https://nodejs.org/api/esm.html#node-addons) | Node 22.20 | flag-injected, never native |

### How it works

> [!NOTE]
> Nub takes advantage of Node extension surfaces that mostly didn't exist when Deno and Bun were built: 
> 
> - [`--import`](https://nodejs.org/api/cli.html#--importmodule)/[`--require`](https://nodejs.org/api/cli.html#-r---require-module) preloads
> - [`module.registerHooks()`](https://nodejs.org/api/module.html#moduleregisterhooksoptions) for transpilation and resolution 
> - [N-API native addons](https://nodejs.org/api/n-api.html): Nub embeds [oxc](https://oxc.rs/) for pre-transpilation

View the [full runtime docs  👉](https://nubjs.com/docs/runtime).

## Script runner — `nub run`

A drop-in for `npm run` and `pnpm run`. The runner is a Rust binary with no JavaScript startup of its own, so it dispatches a warm script roughly 24× faster than `pnpm run`:

```sh
nub run build
nub run -r --filter "@org/*" test     # supports --filter
```

| Command | Time | Relative |
|---|---|---|
| `nub run` | 14.7 ms | — |
| `npm run` | 329.9 ms | 22× |
| `pnpm run` | 442.7 ms | 30× |

> script dispatch · warm · 50 runs · macOS — [View benchmark](https://github.com/nubjs/nub/tree/main/tests/bench/script-runner)

- 🚀 Dispatches a warm script ~30× faster than `pnpm run`
- 🔁 Full lifecycle support — `pre`/`post` hooks and the complete `npm_*` environment
- 🧰 Local `node_modules/.bin` on `PATH`, with args forwarded without the `--` separator
- 🗃️ The full pnpm workspace surface — `-r`, `--filter`, `--parallel`, `--workspace-concurrency`, `--resume-from`, `--stream`
- 🎯 pnpm's `--filter` grammar verbatim — graph (`...@org/web`) and changed-since (`[main]`) selectors

View the [full script runner docs 👉](https://nubjs.com/docs/run).

## Package runner — `nubx` / `nub dlx`

A drop-in for `npx` and `pnpm dlx`. Local-first with a download-and-execute registry fallback (same as `npx`). Eliminating the double-Node.js-spawn performance penalty paid by JavaScript-based tools like `npx` and `pnpm`.

```sh
nubx eslint . --fix
nubx -y cowsay@1.5.0 "hi"   # fetched from the registry (auto-approved via -y)
```

| Command | Time | Relative |
|---|---|---|
| `nubx esbuild --version` | 11 ms | — |
| `pnpm exec esbuild --version` | 191 ms | 17× |
| `npx esbuild --version` | 226 ms | 19× |

> esbuild --version · macOS — [View benchmark](https://github.com/nubjs/nub/tree/main/tests/bench/bin-runner)

- ⚡ Runs a local bin ~19× faster than `npx`, with no Node in the wrapper
- 🔎 Resolves `node_modules/.bin` regardless of which package manager installed it
- 🌐 Registry fallback for uninstalled bins — fetched, run, then discarded
- 🧩 Full `pnpm exec` / `pnpm dlx` flag parity, shell mode included
- 🪜 Walks the resolution chain — member `.bin`, then workspace root, then ancestors

View the [full package runner docs 👉](https://nubjs.com/docs/nubx).

## Package manager — `nub install`

Nub is a package manager powered by the [Aube](https://github.com/jdx/aube) engine. The CLI is 1:1 compatible with `pnpm` for muscle memory. 

```sh
nub install                    # alias: nub i  ·  also: nub ci, --frozen-lockfile
nub add -E -D --save-catalog react
nub remove lodash
nub update
nub dedupe
nub import                     # convert another lockfile in place
```

| Tool | Time | Relative |
|---|---|---|
| `nub` | 1122 ms | — |
| `bun` | 1444 ms | 29% slower |
| `pnpm` | 2847 ms | 2.5× |
| `npm` | 4163 ms | 3.7× |

> warm frozen install · create-t3-app · 222 deps · macOS — [View benchmark](https://github.com/nubjs/nub/tree/main/tests/bench/install)

- ⚡ Installs create-t3-app (222 deps) warm and frozen ~2.5× faster than `pnpm`
- 🔄 Round-trips your existing lockfile — npm, pnpm, and Bun in place; Yarn read-only
- 🧠 Infers the incumbent package manager and mirrors it — no migration, no prompt
- 🗄️ Dedupes through a global content-addressed store, materialized by reflink/hardlink
- 🐍 Accepts pnpm's flags with the same spelling and semantics, down to the workspace catalog
- 🛡️ Treats build scripts as deny-by-default — an explicit allow or a vetted default-trust floor
- 🦠 Blocks OSV malicious-package hits (`MAL-*`) — `ERR_NUB_MALICIOUS_PACKAGE`
- 🔻 Refuses a weakened-provenance downgrade — `trustPolicy=no-downgrade`, `ERR_NUB_TRUST_DOWNGRADE`
- ⏳ Holds back too-new releases — a `minimumReleaseAge` cooling window, 24h by default like pnpm

These supply-chain defenses are on by default, no config required. Dependency build scripts run only on an explicit allow (`pnpm.onlyBuiltDependencies`, `trustedDependencies`, `nub approve-builds`) or when a curated default-trust floor vouches for the package under registry-provenance, advisory-vetting, and cooling-window gates; a skipped package is named with `WARN_NUB_IGNORED_BUILD_SCRIPTS`. An OSV malicious-package hit aborts the install, a weakened-provenance downgrade is refused, and too-new versions wait out the cooling window. See [Package manager](https://nubjs.com/docs/install).

## Package meta-manager — `nub pm`

Corepack's job, in native Rust: provision and run the exact pnpm / npm / yarn your project pins:

```sh
nub pm use pnpm@9.15.4   # declare the project's PM, align the lockfile
nub pm shim              # bare npm/pnpm/yarn route through the pin
```

- 🎯 Reads the pin from `packageManager`, `devEngines`, or Yarn Berry's `yarnPath`
- 📥 Fetches that version integrity-verified, caches it, and runs it on the project's Node
- 🚫 Needs no `corepack enable` and no baked version table

View the [full `nub pm` docs 👉](https://nubjs.com/docs/pm).

## Node version manager — `nub node`

Pin a version and the matching stock Node is fetched from nodejs.org, SHA-256-verified, cached, and run — in the same breath as your code, no second command:

```sh
echo 22 > .node-version
nub hello.ts
```

```
Using Node.js 22.15.0 (resolved from .node-version)
Installed in 9.8s
Hello world!
```

```sh
nub node install 22     # also: ls, uninstall, pin, which
nub node pin lts
```

- 📌 Pins from `.node-version`, `.nvmrc`, or `engines.node`
- 📥 Fetches stock Node from nodejs.org, SHA-256-verified and cached
- 🤝 Provisions on demand, in the same command that runs your code
- 🧭 Falls back to whatever `node` is on your `PATH` when there's no pin

View the [full `nub node` docs 👉](https://nubjs.com/docs/node).


## License

MIT
