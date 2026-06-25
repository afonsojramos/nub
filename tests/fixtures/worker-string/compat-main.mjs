// Plain JS (no TS syntax) so it parses under --node on EVERY Node tier — the floor
// tiers don't strip types, and --node disables nub's transpiler. With augmentation
// off there is no Worker global, so this throws `Worker is not defined` (not a
// SyntaxError), isolating the no-rewrite/no-augmentation signal.
const w = new Worker("./worker.js");
w.onmessage = (e) => { console.log("main-got:" + e.data); w.terminate(); };
