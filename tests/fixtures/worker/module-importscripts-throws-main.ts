// A default-type (module) worker: importScripts must THROW (WHATWG: importScripts
// is classic-only). nub's default worker type is "module" (documented divergence
// from the WHATWG "classic" default — aligns with nub's ESM-first posture). The
// worker reports whether importScripts threw.
const w = new Worker(new URL("./module-importscripts-worker.ts", import.meta.url));
w.onmessage = (e: MessageEvent) => {
  console.log("module-importscripts:" + e.data);
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
setTimeout(() => { console.log("module-importscripts:timeout"); process.exit(1); }, 10000);
