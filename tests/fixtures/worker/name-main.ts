// {name} option flows to both worker.name (main side, WHATWG Worker.name) and
// self.name (worker side, DedicatedWorkerGlobalScope.name).
const w = new Worker(new URL("./name-worker.ts", import.meta.url), { name: "pricer" });
console.log("main-worker.name:" + (w as { name: string }).name);
w.onmessage = (e: MessageEvent) => {
  console.log("self.name:" + e.data);
  (w as { terminate(): void }).terminate();
};
