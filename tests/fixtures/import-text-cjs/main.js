// A `.js` under `type: commonjs` — Node treats it as CommonJS, where an ESM
// `import` statement is a SyntaxError on stock Node and off-floor nub alike. On
// the 18.19 floor the `with`→`assert` rewrite must NOT force this file to run as
// module (that would make it succeed on the floor only). If it ever ran, it would
// print this sentinel; the test asserts it does NOT and that nub exits non-zero.
import s from "./notes.txt" with { type: "text" };
console.log("CJS-CTX-RAN:" + s);
