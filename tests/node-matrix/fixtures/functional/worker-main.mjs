import { Worker } from "node:worker_threads";
import { fileURLToPath } from "node:url";
const w = new Worker(fileURLToPath(new URL("./worker-task.mjs", import.meta.url)));
w.on("message", (m) => { console.log("WORKER:" + m); w.terminate(); });
w.on("error", (e) => { console.error("WORKER_ERR:" + e.message); process.exit(1); });
