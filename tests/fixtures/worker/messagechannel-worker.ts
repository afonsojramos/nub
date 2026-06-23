self.onmessage = (e: MessageEvent) => {
  const port = (e.data as { port: MessagePort }).port;
  port.onmessage = (ev: MessageEvent) => {
    port.postMessage("echo:" + ev.data);
  };
  port.start?.();
};
