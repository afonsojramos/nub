// Classic worker: importScripts loads dep.cjs, which defines globalThis.NUB_DEP.
const path = require("node:path");
importScripts(path.join(__dirname, "classic-dep.cjs"));
self.postMessage("dep-said:" + globalThis.NUB_DEP);
