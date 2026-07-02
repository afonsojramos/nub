// nub's own Import Text: `import s from "./f" with { type: "text" }` yields the
// raw file contents as a default-export string, on ANY extension. The attribute
// takes precedence over both nub's extension-based data loaders (a `.yaml` read
// as text is NOT parsed) and Node-native JSON (a `.json` read as text is the raw
// string, not the parsed object). Default export only — a named import is a
// separate load-error case (import-text-named fixture).
import md from "./notes.md" with { type: "text" };
import yamlText from "./config.yaml" with { type: "text" };
import jsonText from "./data.json" with { type: "text" };

console.log("md:" + JSON.stringify(md));
console.log("yaml-is-string:" + (typeof yamlText === "string"));
console.log("yaml-unparsed:" + yamlText.startsWith("host: db.example.com"));
console.log("json-is-string:" + (typeof jsonText === "string"));
console.log("json-unparsed:" + JSON.stringify(jsonText));
