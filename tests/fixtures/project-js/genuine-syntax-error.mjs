// The other side of the #225 fallback: a GENUINELY broken plain-JS file must still
// fail loudly. The real syntax error (`const x = ;`) means neither oxc nor V8 can run
// it — whichever internal path it takes (early no-op or transpile-then-fallback), the
// original source reaches V8, which surfaces its own SyntaxError rather than masking it.
const re = /a/v;
const x = ;
console.log("UNREACHABLE", re, x);
