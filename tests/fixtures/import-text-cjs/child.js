// A `.js` under `type: commonjs` (see package.json) that carries an ESM
// import-attribute clause. It is genuine CommonJS, so importing it from an ESM
// parent is an error on stock Node and off-floor nub alike. On the 18.19 floor the
// `with`->`assert` keyword rewrite must NOT force this file to load as module (which
// would make it run on the floor ONLY — a cross-version divergence). If it ever ran
// as module it would print this sentinel; the test asserts it does not.
import s from "./notes.txt" with { type: "text" };
export const body = s;
console.log("CJS-CHILD-RAN-AS-MODULE:" + s);
