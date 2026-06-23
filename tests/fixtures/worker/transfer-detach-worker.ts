self.onmessage = (e: MessageEvent) => {
  const view = new Uint8Array(e.data as ArrayBuffer);
  self.postMessage(view.byteLength + ":" + view[7]);
};
