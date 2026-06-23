// postMessage(data, [buffer]) transfers the ArrayBuffer: ownership moves to the
// worker and the main-side buffer is DETACHED (byteLength 0). The worker reports
// the bytes it received; the main side asserts its own buffer is now empty.
const w = new Worker(new URL("./transfer-detach-worker.ts", import.meta.url));
const buf = new ArrayBuffer(8);
new Uint8Array(buf).set([1, 2, 3, 4, 5, 6, 7, 8]);
w.onmessage = (e: MessageEvent) => {
  console.log("worker-saw-bytes:" + e.data);
  console.log("main-detached:" + (buf.byteLength === 0));
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
w.postMessage(buf, [buf]);
setTimeout(() => { console.log("transfer:timeout"); process.exit(1); }, 10000);
