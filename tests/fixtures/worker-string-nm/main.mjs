import { makeWorker } from "dep";
const w = makeWorker();
w.onmessage = (e) => { console.log("UNEXPECTED-dep-resolved:" + e.data); w.terminate(); };
w.onerror = () => { console.log("dep-worker-cwd-relative-as-expected"); process.exit(0); };
