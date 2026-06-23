import { parentPort } from "node:worker_threads";
parentPort.postMessage("WORKER_PONG");
