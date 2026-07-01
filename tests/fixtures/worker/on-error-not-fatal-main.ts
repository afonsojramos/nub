// DUAL-CHANNEL FATALITY: a worker error handled ONLY via the node channel
// (`.on('error')`, which delivers a BARE Error) must NOT be treated as
// unhandled — the parent must survive (exit 0), not throw the fatal that an
// unlistened error produces. The fatality check counts error listeners on BOTH
// the web (addEventListener/onerror) and node (.on('error')) channels.
const w = new Worker(`throw new Error("boom");`, { eval: true });
w.on("error", (err: Error) => {
  console.log("on-error-bare:" + (err instanceof Error) + ":" + err.message);
  // If the fatal fired, the process would die before this tick prints.
  setTimeout(() => {
    console.log("parent-alive:true");
    process.exit(0);
  }, 100);
});
setTimeout(() => {
  console.log("parent-alive:never-handled");
  process.exit(3);
}, 10000);
