// Emits process identity fields on stdout as JSON so the test can assert
// without any text-matching fragility. execPath is expected to be the
// real node binary path (an absolute path, NOT "node") — it must not
// change when nub sets argv0.
const out = {
  title: process.title,
  argv0: process.argv0,
  execPathIsAbsolute: require("path").isAbsolute(process.execPath),
};
process.stdout.write(JSON.stringify(out) + "\n");
