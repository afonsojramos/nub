// A text import exposes ONLY a default export; a named import has no matching
// export and is a load-time SyntaxError (nothing in this module runs).
import { heading } from "./note.md" with { type: "text" };
console.log("should-not-print:" + heading);
