// A non-ASCII worker filename must percent-encode its UTF-8 bytes in the emitted
// URL (café → caf%C3%A9) and round-trip back through new URL → fileURLToPath.
const w = new Worker("./café.ts");
w.onmessage = (e: MessageEvent) => {
  console.log("main-got:" + e.data);
  (w as { terminate(): void }).terminate();
};
export {};
