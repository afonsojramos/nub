const u = import.meta.resolve("./dep.cjs");
console.log("META_RESOLVE:" + (u.startsWith("file:") ? "ok" : "bad"));
