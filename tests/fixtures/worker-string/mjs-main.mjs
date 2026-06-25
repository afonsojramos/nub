// A plain .mjs caller — nub does not transpile it by default, but a bare worker
// string triggers the transform so the rewrite fires, uniformly with .ts callers.
const w = new Worker("./js-worker.js");
w.onmessage = (e) => { console.log("main-got:" + e.data); w.terminate(); };
