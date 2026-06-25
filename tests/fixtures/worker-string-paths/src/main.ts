// A tsconfig `paths` alias resolves like a top-level import + is erased to a
// portable relative URL in the emit (no tsconfig needed at runtime).
const w = new Worker("@workers/w");
w.onmessage = (e: MessageEvent) => {
  console.log("main-got:" + e.data);
  (w as { terminate(): void }).terminate();
};
export {};
