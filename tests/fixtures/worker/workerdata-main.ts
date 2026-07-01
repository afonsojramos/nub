// Regression: workerData is forwarded through the constructor to the worker
// (spread via ...options into the underlying node:worker_threads.Worker), so a
// worker reading worker_threads.workerData sees what the parent passed.
const w = new Worker(new URL("./workerdata-worker.ts", import.meta.url), {
  workerData: { seed: 41 },
});
w.onmessage = (e: MessageEvent) => {
  console.log("workerdata:" + e.data);
  (w as { terminate(): void }).terminate();
};
