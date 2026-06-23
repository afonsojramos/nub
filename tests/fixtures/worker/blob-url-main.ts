// A `blob:` URL worker, the browser-standard inline mechanism. nub wraps
// URL.createObjectURL so the source is captured synchronously for the Worker ctor.
const src = "self.postMessage('from-blob-url')";
const url = URL.createObjectURL(new Blob([src], { type: "text/javascript" }));
const w = new Worker(url);
w.onmessage = (e: MessageEvent) => {
  console.log("blob-url:" + e.data);
  URL.revokeObjectURL(url);
  (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message: string }) => {
  console.log("blob-url-error:" + e.message);
  process.exit(1);
};
