// POSITIVE fixture — every surface @nubjs/types adds over @types/node must resolve
// under the canonical consumer config: lib es2024 (no dom) + types ["node","@nubjs/types"].
// Expected: tsc --noEmit exits 0.

// Data-format import wildcards (declare module "*.yaml" / "*.toml" / …).
import yamlCfg from "./config.yaml";
import tomlCfg from "./config.toml";

// Browser-shape Worker global + its methods/handlers.
const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
worker.postMessage({ yaml: yamlCfg, toml: tomlCfg });
worker.onmessage = (ev) => console.log(ev.data);
worker.onerror = (ev) => console.error(ev);

// Node worker_threads compatibility: EventEmitter methods (node-channel shapes),
// the online/exit lifecycle, an awaited terminate(), and { eval: true }.
worker
  .on("message", (value) => console.log(value)) // raw value, typed `any`
  .on("error", (err: Error) => console.error(err.stack))
  .on("exit", (code: number) => console.log(code))
  .on("online", () => console.log("online"));
worker.once("message", (value) => console.log(value));
worker.off("message", () => {});
const emitted: boolean = worker.emit("message", 1);
void emitted;
const exitCode: Promise<number> = worker.terminate();
void exitCode;
const inlineWorker = new Worker("self.postMessage(1)", { eval: true });
void inlineWorker;

// reportError (WinterTC global; not in @types/node).
reportError(new Error("boom"));

// Temporal namespace (inlined from @js-temporal/polyfill).
const instant: Temporal.Instant = Temporal.Now.instant();
const duration: Temporal.Duration = Temporal.Duration.from({ hours: 2, minutes: 30 });
console.log(instant.toString(), duration.total("minutes"));

// Date.prototype.toTemporalInstant.
const fromDate: Temporal.Instant = new Date().toTemporalInstant();
console.log(fromDate.epochMilliseconds);

// import.meta.hot (undefined unless `nub watch --hot`, but the shape must typecheck).
if (import.meta.hot) {
  import.meta.hot.accept((mod) => console.log(mod));
  import.meta.hot.dispose((data) => console.log(data));
}
