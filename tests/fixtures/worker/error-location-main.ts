// A throwing worker surfaces a spec-correct ErrorEvent on the parent: message,
// error, AND filename/lineno/colno (populated from the worker error's stack,
// not hardcoded empty). We assert a real filename + nonzero lineno.
const w = new Worker(new URL("./error-location-worker.ts", import.meta.url));
w.onerror = (e: { filename?: string; lineno?: number; colno?: number; message?: string }) => {
  const hasFile = typeof e.filename === "string" && e.filename.includes("error-location-worker");
  const hasLine = typeof e.lineno === "number" && e.lineno > 0;
  console.log("err-loc:filename=" + hasFile + ":lineno=" + hasLine);
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
setTimeout(() => { console.log("err-loc:timeout"); process.exit(1); }, 10000);
