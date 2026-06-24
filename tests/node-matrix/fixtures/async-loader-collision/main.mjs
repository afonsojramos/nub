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
// from (or never hits) the resolveSync/loadSync stub. nub's fast-tier hooks DO recover the
// stub throw (the Next.js/Turbopack/Tailwind fix), so this must pass on EVERY version —
// including the previously-broken bands (v22.15-22.16, v23.6-23.11, v24.1-24.11, v25.0-25.1),
// which is exactly where the regression guard has teeth. A crash here (exit != 0,
// `ERR_METHOD_NOT_IMPLEMENTED` on stderr) means nub regressed the recovery or a new broken
// Node band appeared. Node-fixed (v24.12+, v25.2+, v26) and compat-tier (no registerHooks)
// versions pass trivially.
import nodeModule, { register } from "node:module";
import { writeSync } from "node:fs";

// SELF-GUARD (Scenario B must not pass vacuously when nub's augmentation is silently
// ABSENT). The resolveSync collision can only occur on nub's FAST tier — the tier that
// installs SYNC hooks via `module.registerHooks`. nub wraps `registerHooks` with an internal
// `__nubWrapped` sentinel (runtime/preload-common.cjs). So on any Node that HAS registerHooks
// (>=22.15, the fast-tier floor), a MISSING sentinel means nub's preload never loaded — the
// leg would otherwise "pass" the collision for the wrong reason (no sync hook = no crash, but
// also = nothing under test). Fail loudly instead. (On the compat tier registerHooks does not
// exist and the sync-hook collision is structurally impossible — nothing to guard.)
if (typeof nodeModule.registerHooks === "function" && nodeModule.registerHooks.__nubWrapped !== true) {
  writeSync(2, "COLLISION_FAIL: nub fast-tier augmentation is NOT active (registerHooks not wrapped) — preload did not load; the collision is not under test on this leg\n");
  process.exit(3);
}

register("./loader.mjs", import.meta.url);

// Also exercise a post-register resolution (the in-the-wild trigger also resolves further
// modules after the loader is live). Both paths go through the sync chain.
const { pathToFileURL } = await import("node:url");
if (typeof pathToFileURL !== "function") {
  writeSync(2, "COLLISION_FAIL: node:url import returned wrong shape\n");
  process.exit(2);
}

writeSync(1, "COLLISION_OK\n");
