// Module worker: importScripts must throw.
let threw = false;
try {
  (globalThis as unknown as { importScripts: (u: string) => void }).importScripts("data:text/javascript,1");
} catch {
  threw = true;
}
self.postMessage(threw ? "threw" : "did-not-throw");
