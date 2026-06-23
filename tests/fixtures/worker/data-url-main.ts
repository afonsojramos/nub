// A `data:` URL worker — a WHATWG inline-worker mechanism. nub's worker-side
// scope (self/postMessage) is installed via the preload even for a data: worker.
const src = "self.postMessage('from-data-url')";
const w = new Worker("data:text/javascript," + encodeURIComponent(src));
w.onmessage = (e: MessageEvent) => {
  console.log("data-url:" + e.data);
  (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message: string }) => {
  console.log("data-url-error:" + e.message);
  process.exit(1);
};
