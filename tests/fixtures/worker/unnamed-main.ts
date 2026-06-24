// An UNNAMED worker's self.name must be "" (WHATWG), NOT Node's "WorkerThread"
// thread-display-name sentinel (which native worker_threads.threadName returns on
// v24.6+/v22.20+ for a worker spawned without {name}).
const w = new Worker(new URL("./name-worker.ts", import.meta.url));
w.onmessage = (e: MessageEvent) => {
  console.log("unnamed-self.name:[" + e.data + "]");
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
setTimeout(() => { console.log("unnamed:timeout"); process.exit(1); }, 10000);
