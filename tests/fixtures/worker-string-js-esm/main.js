// A `.js` in a type:module package is ESM but not transpiled by default — the
// worker trigger routes it through the transform so the rewrite fires.
const w = new Worker("./worker.js");
w.onmessage = (e) => { console.log("main-got:" + e.data); w.terminate(); };
