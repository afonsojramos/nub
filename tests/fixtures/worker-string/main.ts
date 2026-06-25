// A BARE relative string (no `new URL`) must resolve against THIS module — like a
// top-level import — not process.cwd(). Run from a foreign cwd, this reaches the
// sibling worker.ts only because nub rewrote the specifier caller-relative.
const w = new Worker("./worker.ts");
w.onmessage = (e: MessageEvent) => {
  console.log("main-got:" + e.data);
  (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message: string }) => {
  console.log("worker-error:" + e.message);
  process.exit(1);
};
export {};
