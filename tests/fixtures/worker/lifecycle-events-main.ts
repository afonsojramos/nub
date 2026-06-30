// Additive node lifecycle: `online` and `exit` fire on the parent handle as
// EventTarget `Event` objects (NOT the node EventEmitter raw-arg form), and
// `exit` carries the worker's exit code as a property. A self.close() exits 0.
const w = new Worker(new URL("./lifecycle-events-worker.ts", import.meta.url));
let online = false;
w.addEventListener("online", () => {
  online = true;
});
w.addEventListener("exit", (ev: Event & { code?: number }) => {
  console.log("online-fired:" + online);
  console.log("exit-is-event:" + (ev instanceof Event) + ":code=" + ev.code);
  process.exit(0);
});
// Backstop: only fires if the exit event never arrives (a real regression).
setTimeout(() => {
  console.log("no-exit-event");
  process.exit(2);
}, 10000);
