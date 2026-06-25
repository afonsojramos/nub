// `Worker` is BOUND to node:worker_threads here → must NOT be rewritten. A bound
// reference has a symbol_id, so the scope guard skips it. We pass a RELATIVE path
// (not absolute, which would be passthrough either way) to genuinely exercise the
// guard: worker_threads resolves it against cwd, so running this from the fixture
// dir finds ./wt-worker.cjs. If nub had wrongly rewritten the bound Worker to the
// caller-relative `new URL(...)` form, the spawn target would differ — this proves
// it left the worker_threads binding alone.
import { Worker } from "node:worker_threads";
const w = new Worker("./wt-worker.cjs");
w.on("message", (m: string) => {
  console.log("main-got:" + m);
  w.terminate();
});
export {};
