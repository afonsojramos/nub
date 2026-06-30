// `new Worker(code, { eval: true })` — additive node-mirroring inline source.
// On the fast tier (Node 22.15+) the worker-side web scope (self/postMessage)
// is installed, so the inline source replies over `self`. (A compat-tier eval
// worker has no `self` — it reaches node:worker_threads.parentPort directly;
// that tier gap is documented in web-worker.md and the docs page.)
const w = new Worker(`self.onmessage = (e) => self.postMessage(e.data + 1);`, {
  eval: true,
});
w.onmessage = (e: MessageEvent) => {
  console.log("eval:" + e.data);
  (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message?: string }) => {
  console.log("eval-error:" + e.message);
  process.exit(1);
};
w.postMessage(41);
