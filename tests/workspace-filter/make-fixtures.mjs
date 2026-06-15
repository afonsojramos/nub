#!/usr/bin/env node
// Build the differential `--filter`/`-r` fixtures: one real workspace per
// topology, written under a destination dir (default /tmp/nub-wsfilter-fixtures).
//
// Each topology is a directory containing:
//   - package.json        with `workspaces` (nub's native member source)
//   - pnpm-workspace.yaml  (pnpm's native member source — identical globs)
//   - one package.json per member, wiring real workspace: deps, every member
//     carrying an identical `whoami` script that prints `NUBPKG:<name>`.
//
// Both tools discover the SAME member set from their own native config, so a
// selector run against each yields directly comparable selected-package sets.
// We deliberately give each fixture both config files (rather than relying on
// nub reading pnpm-workspace.yaml — which is brand-gated behind an incumbent
// pnpm-lock.yaml) so the two engines read equivalent, native inputs.
//
// No install is run: the membership oracle (`-r run whoami`) never needs the
// dependency trees materialized in node_modules — pnpm and nub both compute the
// selection from the manifests alone. That keeps the harness fast and offline.

import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";

// The whoami script: prints a greppable sentinel + the package's own name. The
// single-quoted-in-JSON form survives both pnpm's and nub's shell invocation.
const WHOAMI =
  `node -e "process.stdout.write('NUBPKG:'+require(process.cwd()+'/package.json').name+'\\n')"`;

// A topology is { name, globs, packages: { <name>: [deps...] | {deps,devDeps} } }.
// `globs` are the workspace patterns; `packages` maps a member name to either a
// plain dependency list or an object splitting prod/dev deps. The member's
// directory is derived from the first matching glob family (see `dirFor`).
const TOPOLOGIES = [
  {
    name: "linear",
    doc: "a→b→c→d dependency chain — the ellipsis-direction core case",
    globs: ["packages/*"],
    dir: (n) => `packages/${n}`,
    packages: { a: ["b"], b: ["c"], c: ["d"], d: [] },
  },
  {
    name: "diamond",
    doc: "a→{b,c}→d — shared transitive dep, dependents/dependencies fan",
    globs: ["packages/*"],
    dir: (n) => `packages/${n}`,
    packages: { a: ["b", "c"], b: ["d"], c: ["d"], d: [] },
  },
  {
    name: "wide-fan",
    doc: "root→leaf1..leaf6 — one hub depending on many independent leaves",
    globs: ["packages/*"],
    dir: (n) => `packages/${n}`,
    packages: {
      hub: ["leaf1", "leaf2", "leaf3", "leaf4", "leaf5", "leaf6"],
      leaf1: [], leaf2: [], leaf3: [], leaf4: [], leaf5: [], leaf6: [],
    },
  },
  {
    name: "islands",
    doc: "two disjoint sub-graphs (x1→x2, y1→y2) that never reference each other",
    globs: ["packages/*"],
    dir: (n) => `packages/${n}`,
    packages: { x1: ["x2"], x2: [], y1: ["y2"], y2: [] },
  },
  {
    name: "nested-dirs",
    doc: "packages/* and apps/* — dir-structured members for {dir}/glob selectors",
    globs: ["packages/*", "apps/*"],
    // core/util live under packages/, web/api under apps/.
    dir: (n) => (n === "web" || n === "api" ? `apps/${n}` : `packages/${n}`),
    packages: { core: [], util: ["core"], web: ["util", "core"], api: ["core"] },
  },
  {
    name: "dev-prod-mix",
    doc: "app prod-depends lib, dev-depends tool — exercises dev/prod dep edges",
    globs: ["packages/*"],
    dir: (n) => `packages/${n}`,
    // app: prod dep on lib, dev dep on tool. nub's dep graph walks
    // dependencies+devDependencies+peerDependencies (matching pnpm's default
    // selection, which includes dev edges), so both should pull tool via app.
    packages: {
      app: { deps: ["lib"], devDeps: ["tool"] },
      lib: { deps: [] },
      tool: { deps: [] },
    },
  },
];

function depBlock(spec) {
  // Normalize a package spec to { dependencies, devDependencies }.
  const deps = Array.isArray(spec) ? spec : spec.deps ?? [];
  const devDeps = Array.isArray(spec) ? [] : spec.devDeps ?? [];
  const toObj = (names) =>
    Object.fromEntries(names.map((d) => [d, "workspace:*"]));
  const out = {};
  if (deps.length) out.dependencies = toObj(deps);
  if (devDeps.length) out.devDependencies = toObj(devDeps);
  return out;
}

function writeTopology(root, topo) {
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });

  // Root manifest: workspaces for nub. `private` so neither tool warns.
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify(
      { name: `root-${topo.name}`, private: true, workspaces: topo.globs },
      null,
      2,
    ) + "\n",
  );
  // pnpm-workspace.yaml for pnpm — identical globs.
  writeFileSync(
    join(root, "pnpm-workspace.yaml"),
    "packages:\n" + topo.globs.map((g) => `  - '${g}'`).join("\n") + "\n",
  );

  for (const [name, spec] of Object.entries(topo.packages)) {
    const rel = topo.dir(name);
    const dir = join(root, rel);
    mkdirSync(dir, { recursive: true });
    const manifest = {
      name,
      version: "1.0.0",
      ...depBlock(spec),
      scripts: { whoami: WHOAMI },
    };
    writeFileSync(
      join(dir, "package.json"),
      JSON.stringify(manifest, null, 2) + "\n",
    );
  }
}

const dest = process.argv[2] || "/tmp/nub-wsfilter-fixtures";
rmSync(dest, { recursive: true, force: true });
mkdirSync(dest, { recursive: true });

for (const topo of TOPOLOGIES) {
  writeTopology(join(dest, topo.name), topo);
}

// Emit a manifest the runner reads to know which topologies exist + their docs.
writeFileSync(
  join(dest, "topologies.json"),
  JSON.stringify(
    TOPOLOGIES.map((t) => ({
      name: t.name,
      doc: t.doc,
      members: Object.keys(t.packages),
    })),
    null,
    2,
  ) + "\n",
);

console.error(`wrote ${TOPOLOGIES.length} topologies to ${dest}`);
for (const t of TOPOLOGIES) {
  console.error(`  ${t.name.padEnd(13)} — ${t.doc}`);
}
