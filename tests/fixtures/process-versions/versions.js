// Emits the nub self-identification marker on stdout as JSON so the test can
// assert without text-matching fragility. `nub` is null when the marker is
// absent (e.g. under `--node`/`NODE_COMPAT`, where the preload doesn't run).
// `desc` captures the property shape so the test can verify it mirrors Node's
// own `process.versions` entries (enumerable, configurable, non-writable).
const d = Object.getOwnPropertyDescriptor(process.versions, "nub");
const out = {
  nub: process.versions.nub ?? null,
  desc: d ? { writable: d.writable, enumerable: d.enumerable, configurable: d.configurable } : null,
};
process.stdout.write(JSON.stringify(out) + "\n");
