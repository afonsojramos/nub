// A worker whose module FAILS TO RESOLVE, with NO 'error'/onerror listener. The
// oracle (node:worker_threads.Worker, an EventEmitter) re-throws an unhandled
// 'error' on the main thread → uncaughtException → nonzero exit + printed stack.
// nub's browser-shape Worker is an EventTarget, whose dispatchEvent silently
// drops an unlistened event — so a failed worker load USED to exit 0 in total
// silence (the maintainer-found bug). The fix reproduces the oracle's fatality.
new Worker(new URL("./does-not-exist.mjs", import.meta.url), { type: "module" });

// Keep the loop alive so the async worker-load failure has time to arrive. If the
// swallow bug were present, the process would exit 0 and print this line; with the
// fix the unhandled error is fatal before the timer ever fires.
setTimeout(() => {
  console.log("swallowed-and-survived");
  process.exit(0);
}, 5000);
