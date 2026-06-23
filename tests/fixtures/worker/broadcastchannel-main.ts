// BroadcastChannel is a Node global (stable v18.0). Two channels on the same name
// in the same process: a message from one is received by the OTHER, and the
// sender does NOT receive its own (WHATWG §9.5).
const tx = new BroadcastChannel("nub-bc-test");
const rx = new BroadcastChannel("nub-bc-test");
let txGotOwn = false;
tx.onmessage = () => { txGotOwn = true; };
rx.onmessage = (e: MessageEvent) => {
  console.log("bc-received:" + e.data);
  console.log("bc-sender-got-own:" + txGotOwn);
  tx.close();
  rx.close();
  process.exit(0);
};
tx.postMessage("broadcast-hello");
setTimeout(() => { console.log("bc:timeout"); process.exit(1); }, 10000);
