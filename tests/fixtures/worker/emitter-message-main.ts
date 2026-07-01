// EventEmitter delegation, web channel stays primary: `.on('message', fn)`
// delivers the RAW value (node channel), while `addEventListener('message')`
// still delivers a `MessageEvent` (web channel, unchanged). Chainable `.on`
// returns the wrapper, not the inner node Worker. The eval worker uses
// `parentPort` (portable across tiers) so the test turns on the delegation, not
// the worker-side `self` scope.
const w = new Worker(
  `const { parentPort } = require("node:worker_threads");
   parentPort.on("message", (data) => parentPort.postMessage({ echo: data }));`,
  { eval: true },
);
let web = false;
let node = false;
w.addEventListener("message", (ev) => {
  console.log("web-is-messageevent:" + (ev instanceof MessageEvent));
  web = true;
  done();
});
const ret = w.on("message", (val: { echo?: string }) => {
  console.log("node-is-messageevent:" + (val instanceof MessageEvent));
  console.log("node-raw-echo:" + val.echo);
  node = true;
  done();
});
console.log("on-chain-returns-worker:" + (ret === w));
function done() {
  if (web && node) (w as { terminate(): void }).terminate();
}
w.postMessage("ping");
