const w = new Worker("./worker"); // no extension — nub probes to ./worker.ts
w.onmessage = (e: MessageEvent) => {
  console.log("main-got:" + e.data);
  (w as { terminate(): void }).terminate();
};
export {};
