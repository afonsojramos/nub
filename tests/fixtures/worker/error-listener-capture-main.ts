// A live BUBBLE-phase 'error' listener is present while a CAPTURE-phase
// registration of the SAME function is removed. EventTarget listener identity is
// (type, callback, capture), so these are distinct registrations; the unhandled-
// 'error' fatality must key on that identity and NOT treat the Worker as
// listener-less here. The handler must fire and the process must survive (exit 0).
const fn = (e: { message?: string; error?: { message?: string } }) => {
  console.log("capture-bubble-handled:" + (e.message ?? e.error?.message ?? ""));
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
const w = new Worker(new URL("./throwing-worker.ts", import.meta.url));
(w as unknown as EventTarget).addEventListener("error", fn, true); // capture
(w as unknown as EventTarget).addEventListener("error", fn, false); // bubble (live)
(w as unknown as EventTarget).removeEventListener("error", fn, true); // remove capture only

setTimeout(() => {
  console.log("no-handler-fired");
  process.exit(2);
}, 10000);
