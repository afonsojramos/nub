// Inbound-event `target`/`currentTarget` identity (WHATWG §DedicatedWorkerGlobalScope).
// An inbound `message` event dispatched to the worker global must have
// `event.target === self` and `event.currentTarget === self` DURING the handler,
// observed identically via `addEventListener('message', …)` and `self.onmessage`.
// Pre-fix, nub hand-invoked the listener with a constructed MessageEvent that never
// went through EventTarget dispatch, so both were `null`.
let viaAdd = false;
self.addEventListener("message", (e: MessageEvent) => {
  viaAdd = true;
  self.postMessage({
    ch: "add",
    target: e.target === self,
    currentTarget: e.currentTarget === self,
    type: e.type,
    data: e.data,
  });
});
self.onmessage = (e: MessageEvent) => {
  self.postMessage({
    ch: "onmessage",
    target: e.target === self,
    currentTarget: e.currentTarget === self,
    type: e.type,
    data: e.data,
    sawAdd: viaAdd, // ordering: addEventListener registered first must fire first
  });
};
