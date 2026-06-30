// `new Worker(code, { eval: true })` — additive node-mirroring inline source,
// gated strictly on the option. The worker uses `node:worker_threads.parentPort`
// (the portable idiom, identical across every tier) so the test verifies eval
// alone, not the tier-dependent worker-side `self` scope.
const w = new Worker(
  `const { parentPort } = require("node:worker_threads");
   parentPort.on("message", (n) => parentPort.postMessage(n + 1));`,
  { eval: true },
);
w.onmessage = (e: MessageEvent) => {
  console.log("eval:" + e.data);
  (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message?: string }) => {
  console.log("eval-error:" + e.message);
  process.exit(1);
};
w.postMessage(41);
