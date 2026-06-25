// A no-worker plain .mjs. If nub transpiled it, oxc codegen would normalize the
// single quotes to double, drop the trailing comma, and add a sourcemap footer.
// We read OUR OWN on-disk source — which is always the raw file — but the real
// proof is behavioral: this file must run on Node's NATIVE path (nub returns null
// from maybeTranspilePlainJs), so it executes verbatim. The marker confirms it ran.
const   obj = { a:1, b:2, };
const s = 'hi'
console.log('byte-parity:' + obj.a + obj.b + s)
