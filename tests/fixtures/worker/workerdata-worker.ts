import { workerData } from "node:worker_threads";
self.postMessage("seed:" + workerData.seed);
