// Async-loader × sync-hooks collision — the FAITHFUL, Turbopack-free regression guard
// for the Next.js/Turbopack/Tailwind-v4 resolveSync() crash (the class PR #98 targets).
//
// Mechanism: nub's fast tier (Node >=22.15) installs a SYNC resolve hook via
// `module.registerHooks`. When user code then calls `module.register(<async loader>)`,
// Node must resolve the loader module's own specifier ("./loader.mjs") synchronously
// through that sync chain. On Node-broken versions the chain reaches
// `#resolveAndMaybeBlockOnLoaderThread` with an async-loader `#customizations` set and
// calls `Hooks.resolveSync()` — a stub that throws `ERR_METHOD_NOT_IMPLEMENTED`. The
// throw aborts the process. (Reduced from `@tailwindcss/node`; no bundler required —
// the bare `module.register()` call is sufficient, verified through the real nub binary.)
//
// PASS contract: this program prints "COLLISION_OK" and exits 0 ONLY when nub recovers
// from (or never hits) the resolveSync stub. On a Node-broken version with an un-fixed
// nub it crashes BEFORE printing — exit != 0, `ERR_METHOD_NOT_IMPLEMENTED` on stderr.
//
// IMPORTANT: this guard is only meaningful on a Node-BROKEN version (v22.15-22.16,
// v23.6-23.11, v24.1-24.11.x, v25.0-25.1). On Node-fixed versions (v24.12+, v25.2+, v26)
// it is vacuously green. The matrix MUST run it on at least one broken-tier leg.
import { register } from "node:module";
import { writeSync } from "node:fs";

register("./loader.mjs", import.meta.url);

// Also exercise a post-register resolution (the in-the-wild trigger also resolves further
// modules after the loader is live). Both paths go through the sync chain.
const { pathToFileURL } = await import("node:url");
if (typeof pathToFileURL !== "function") {
  writeSync(2, "COLLISION_FAIL: node:url import returned wrong shape\n");
  process.exit(2);
}

writeSync(1, "COLLISION_OK\n");
