// terminate() now RETURNS the underlying node:worker_threads Promise<exitCode>
// (additive void→value widening) instead of discarding it. Assert it is a
// thenable resolving to a numeric exit code; the exact code for a forced
// terminate is Node's to define, so we pin the TYPE, not the value.
const w = new Worker(`setInterval(() => {}, 1000);`, { eval: true });
setTimeout(async () => {
  const ret = (w as { terminate(): Promise<number> }).terminate();
  console.log("terminate-thenable:" + (typeof ret?.then === "function"));
  const code = await ret;
  console.log("terminate-code-is-number:" + (typeof code === "number"));
  process.exit(0);
}, 300);
