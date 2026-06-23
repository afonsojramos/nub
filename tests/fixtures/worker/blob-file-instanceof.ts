// Regression: nub wraps globalThis.Blob (to support blob: workers) by subclassing
// it. `File` is a bootstrap global that extends the NATIVE Blob, so the wrap must
// re-point File's prototype chain or `new File(...) instanceof Blob` silently
// becomes false — an additivity violation vs vanilla Node (File IS-A Blob).
const f = new File(["x"], "a.txt", { type: "text/plain" });
console.log("file-instanceof-blob:" + (f instanceof Blob));
// And a Blob made through the wrapper is still a Blob with the full API.
const b = new Blob(["hello"], { type: "text/plain" });
console.log("blob-instanceof-blob:" + (b instanceof Blob));
console.log("blob-size:" + b.size);
