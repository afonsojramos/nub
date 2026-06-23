// MessageChannel/MessagePort are Node globals; verify they behave web-correctly
// under nub: transfer port2 to a worker, exchange a message over the channel.
const w = new Worker(new URL("./messagechannel-worker.ts", import.meta.url));
const { port1, port2 } = new MessageChannel();
port1.onmessage = (e: MessageEvent) => {
  console.log("port-roundtrip:" + e.data);
  port1.close();
  (w as { terminate(): void }).terminate();
  process.exit(0);
};
w.postMessage({ port: port2 }, [port2]);
port1.postMessage("ping-over-port");
setTimeout(() => { console.log("port:timeout"); process.exit(1); }, 10000);
