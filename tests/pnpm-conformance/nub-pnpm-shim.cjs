#!/usr/bin/env node
// Seam-swap shim for pnpm's own black-box test suite (CommonJS form).
//
// pnpm/test/utils/execPnpm.ts spawns every command under test as
// `process.execPath [pnpmBinLocation, ...args]`, where pnpmBinLocation is the
// pnpm package's bin entry (pnpm/bin/pnpm.cjs on older pnpm, pnpm.mjs on newer).
// The conformance runner copies THIS file over that path so every test that
// "runs pnpm" instead runs the nub binary, identified as pnpm (nub picks its
// package-manager identity from argv[0]'s basename — Argv0::detect in
// crates/nub-cli/src/cli.rs). nub-as-pnpm is exactly the drop-in surface the
// suite asserts on (stdout/stderr/exit/lockfile/node_modules).
//
// RE-ENTRY GUARD (why this is not a one-liner): the swap makes `pnpm` resolve
// to this shim EVERYWHERE on a `node_modules/.bin` PATH in the monorepo, not
// just at the test seam — jest-config declares `pnpm: workspace:*`, so its
// `.bin/pnpm` also points here. nub-as-pnpm runs lifecycle scripts with `.bin`
// on PATH, so a nested `pnpm` (a lifecycle script, dlx, or runtime provision)
// would re-enter nub install → unbounded self-spawn (the fork bomb). The fix:
// only the FIRST entry is the command-under-test (nub); a NESTED entry execs
// the REAL pnpm instead, so nested infra `pnpm` terminates exactly as it would
// in a real-pnpm run. First-vs-nested is carried by a sentinel env var that the
// first entry stamps and a nested entry detects.
//
// Written as .cjs so it loads regardless of the package's "type" field. The
// nub path, the pristine-pnpm backup path, and the clone dir are baked in at
// swap time (pnpm's createEnv() keeps only PATH/COLORTERM/APPDATA, so exported
// env would not survive to the spawned shim).
'use strict'
const { spawnSync } = require('node:child_process')
const fs = require('node:fs')
const path = require('node:path')

const SENTINEL = '__NUB_PNPM_SEAM'

// The runner replaces these placeholders at swap time. They MUST be baked (not
// read from env) because pnpm's harness rebuilds a clean env in createEnv().
const BAKED_NUB_BIN = '__NUB_BIN__'
const BAKED_ORIG_PNPM = '__ORIG_PNPM__'
const BAKED_CLONE_DIR = '__CLONE_DIR__'

const args = process.argv.slice(2)

function finish(res, label) {
  if (res.error) {
    console.error(`nub-pnpm-shim: failed to exec ${label}: ${res.error.message}`)
    process.exit(2)
  }
  process.exit(res.status != null ? res.status : res.signal ? 1 : 0)
}

// Does this file look like the swapped seam (so we never recurse into it when
// hunting for real pnpm)? The .cjs seam IS this shim; the .mjs seam is an ESM
// wrapper that requires nub-pnpm-shim.cjs — both contain this marker string.
function isSwappedSeam(file) {
  try {
    return fs.readFileSync(file, 'utf8').includes('nub-pnpm-shim')
  } catch {
    return false
  }
}

// Locate a REAL pnpm to exec for a nested invocation. Order: the pristine
// backup the runner saved alongside the seam (the exact built front-door pnpm,
// same version — the correct answer), then a global pnpm on PATH that lives
// OUTSIDE the clone (so a monorepo `.bin/pnpm` that points back at the swapped
// seam is never chosen). The backup cannot be trusted blindly — a stale clone
// reuse once left it itself a shim — so it is content-verified before use.
function findRealPnpm() {
  if (!BAKED_ORIG_PNPM.startsWith('__ORIG') && fs.existsSync(BAKED_ORIG_PNPM) && !isSwappedSeam(BAKED_ORIG_PNPM)) {
    return { kind: 'node', file: BAKED_ORIG_PNPM }
  }
  const seamBinDir = path.resolve(__dirname)
  const cloneDir = BAKED_CLONE_DIR.startsWith('__CLONE') ? null : path.resolve(BAKED_CLONE_DIR)
  const names = process.platform === 'win32' ? ['pnpm.exe', 'pnpm.cmd', 'pnpm'] : ['pnpm']
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (!dir) continue
    const resolved = path.resolve(dir)
    if (resolved === seamBinDir) continue
    if (cloneDir && resolved.startsWith(cloneDir + path.sep)) continue
    for (const name of names) {
      const cand = path.join(dir, name)
      if (fs.existsSync(cand) && !isSwappedSeam(cand)) return { kind: 'bin', file: cand }
    }
  }
  return null
}

// NESTED invocation: a `pnpm` spawned from within a nub-as-pnpm run. Defer to
// real pnpm (keeping the sentinel set so any FURTHER nesting also stays real,
// exactly as a real-pnpm run would).
if (process.env[SENTINEL]) {
  const real = findRealPnpm()
  if (!real) {
    console.error('nub-pnpm-shim: nested pnpm invocation but no real pnpm found (backup missing/corrupt and none on PATH)')
    process.exit(2)
  }
  const argv = real.kind === 'node' ? [real.file, ...args] : args
  const cmd = real.kind === 'node' ? process.execPath : real.file
  finish(spawnSync(cmd, argv, { stdio: 'inherit', argv0: 'pnpm', env: process.env }), `real pnpm (${real.file})`)
}

// FIRST entry: this IS the command under test. Run nub-as-pnpm, stamping the
// sentinel so any nested pnpm it spawns takes the real-pnpm branch above.
const nubBin = BAKED_NUB_BIN.startsWith('__NUB') ? process.env.NUB_BIN : BAKED_NUB_BIN
if (!nubBin) {
  console.error('nub-pnpm-shim: nub binary path is not set (neither baked nor NUB_BIN)')
  process.exit(2)
}
finish(
  spawnSync(nubBin, args, {
    stdio: 'inherit',
    argv0: 'pnpm',
    env: { ...process.env, [SENTINEL]: '1' },
  }),
  `nub (${nubBin})`,
)
