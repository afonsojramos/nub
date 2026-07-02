// Floor (Node 18.19.x) plain-JS path. A `.mjs` whose only floor-incompatible
// construct is a `with {…}` import-attribute clause is NOT routed through the
// transpiler; nub minimal-splices the keyword to `assert` so 18.19's V8 can parse it.
// A `with {` inside a string literal must survive the rewrite untouched.
import md from "./notes.md" with { type: "text" };

const decoy = "import q from 'z' with { type }";

console.log("plain-md:" + JSON.stringify(md));
console.log("plain-strlit-ok:" + (decoy === "import q from 'z' with { type }"));
