// ESM entry that imports the `type: commonjs` child — the load-hook path where the
// "force module on the floor" divergence manifests (an entry-invoked CJS .js instead
// errors the same way pre- and post-fix, so it would not discriminate the fix).
import { body } from "./child.js";
console.log("PARENT-GOT:" + body);
