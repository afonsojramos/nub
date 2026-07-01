// Drives event-target-self-worker.ts: post one message, collect the worker's two
// replies (one per inbound-listener channel), print each channel's target/
// currentTarget identity so the harness can assert `=== self` on both.
const w = new Worker(new URL("./event-target-self-worker.ts", import.meta.url));
let seen = 0;
w.onmessage = (e: MessageEvent) => {
  const r = e.data as {
    ch: string;
    target: boolean;
    currentTarget: boolean;
    type: string;
    data: unknown;
    sawAdd?: boolean;
  };
  console.log(
    `${r.ch}:target=${r.target}:currentTarget=${r.currentTarget}:type=${r.type}:data=${r.data}` +
      (r.ch === "onmessage" ? `:sawAdd=${r.sawAdd}` : ""),
  );
  if (++seen === 2) (w as { terminate(): void }).terminate();
};
w.onerror = (e: { message: string }) => {
  console.log("worker-error:" + e.message);
  process.exit(1);
};
w.postMessage("ping");
