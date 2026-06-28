// #225: the `v`-flag RegExp flags this file "transformable", routing it through
// nub's transpiler; oxc's stricter ES grammar then rejects `set choices(choices =
// [])` (a setter with a default param — V8 tolerates it, the spec forbids it). The
// graceful fallback must run the ORIGINAL source instead of hard-crashing, exactly
// as pnpm 11.x's bundled `pnpm.mjs` requires.
const re = /a/v;
class C {
  set choices(choices = []) {
    this._c = choices;
  }
}
const c = new C();
c.choices = undefined;
console.log("FALLBACK-RAN:" + re.test("a") + ":" + JSON.stringify(c._c));
