// CommonJS caller. The Worker rewrite is ESM-only (import.meta), so nub must NOT
// transpile/rewrite this — it runs on Node's native CJS path, byte-identical, and
// the worker string keeps worker_threads' cwd-relative behavior. Distinctive
// formatting (single quotes, trailing comma) would be normalized if nub wrongly
// codegen'd it.
const w = new Worker('./cjs-worker.js');
const o = { a: 1, };
w.onmessage = (e) => { console.log('cjs-got:' + e.data + ':' + o.a); w.terminate(); };
