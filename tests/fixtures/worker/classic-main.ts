// A classic worker ({type:"classic"}) can use importScripts() to synchronously
// load + run another script in its scope (WHATWG). A .cjs entry keeps Node in
// classic/CJS mode so importScripts is the right tool (no ESM import).
const w = new Worker(new URL("./classic-worker.cjs", import.meta.url), { type: "classic" });
w.onmessage = (e: MessageEvent) => {
  console.log("classic:" + e.data);
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
w.onerror = (e: { message: string }) => {
  console.log("classic-error:" + e.message);
  process.exit(1);
};
setTimeout(() => { console.log("classic:timeout"); process.exit(1); }, 10000);
