// Worker entry in TS — the `enum` is non-erasable, so it only runs if the worker
// thread's transpile hook is active (proves the resolved sibling actually ran).
enum Status { Ready = "ready" }
self.postMessage("worker-string:" + Status.Ready);
