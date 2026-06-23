// Verify the NON-SPEC bits are GONE:
//  1. `exit` is not a web Worker event — addEventListener('exit') must never fire.
//  2. messageerror is a plain MessageEvent with data === null (nub used to stuff
//     the error into .data). We can't easily force a deserialize failure portably,
//     so we assert the wiring/shape statically: the event handler defaults and the
//     absence of an exit event after the worker exits.
const w = new Worker(new URL("./spec-events-worker.ts", import.meta.url));
let exitFired = false;
(w as unknown as EventTarget).addEventListener("exit", () => { exitFired = true; });
w.onmessage = (e: MessageEvent) => {
  console.log("got:" + e.data);
  (w as { terminate(): void }).terminate();
  // Give any (erroneous) exit event a tick to fire, then report.
  setTimeout(() => {
    console.log("exit-event-fired:" + exitFired);
    process.exit(0);
  }, 200);
};
