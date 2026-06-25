const { parentPort } = require("node:worker_threads");
parentPort.postMessage("wt-ran");
