# Build-jail empirical validation (macOS)

Validates the build-jail's **net-deny-all + prefetch** default against real
native packages: a lifecycle script run under the jail must SUCCEED when its
prebuilt artifact is already cached (prefetch warmed it outside the jail) and
FAIL only on the network step when the cache is cold. Plus the attack-containment
half: a malicious script can't exfiltrate, read secrets, write outside its
package dir, or see scrubbed secret env vars.

macOS-only (Seatbelt). The CI-runnable, hermetic, network-free distillation of
the thesis lives in `../e2e_prefetch_macos.rs` (warm-offline-build succeeds /
cold-net-fetch blocked, with the net-lifted leg as the negative control proving
net is the sole blocker) and the attack coverage in `../e2e_macos.rs`. THIS dir
documents the real-package loop, which needs npm + network and so is not a unit
test.

## The reproducer

`examples/jail_run.rs` runs an arbitrary command under the real
`script_sandbox::policy` for a given package dir. Net is fully denied (the egress
proxy is unwired, so `net.enforce` ⇒ `(deny network*)` regardless of the allow
list). `--no-net-enforce` / `--no-env-scrub` lift one axis to prove which axis
caused a result.

```
cargo build -p nub-sandbox --example jail_run --profile fast
# warm (caches populated by a normal npm install) — expect success, no network:
jail_run --pkg <pkgdir> --root <projroot> -- <lifecycle cmd>
# cold (cache + built artifact cleared) — expect failure on the network step:
jail_run --pkg <pkgdir> --root <projroot> -- <lifecycle cmd>
# cold + net lifted — expect success, proving net was the only blocker:
jail_run --pkg <pkgdir> --root <projroot> --no-net-enforce -- <lifecycle cmd>
```

## Real-package matrix (node 26.2.0 / darwin-arm64, 2026-06-26)

| package | mechanism | warm jailed | cold jailed | cold + net lifted |
| --- | --- | --- | --- | --- |
| esbuild@0.23.1 | postinstall `node install.js`, platform optionalDep + registry fallback | SUCCESS | FAIL — `getaddrinfo ENOTFOUND registry.npmjs.org`, socket denied | SUCCESS (downloads tgz) |
| better-sqlite3@12.11.1 | `prebuild-install \|\| node-gyp rebuild`, GitHub-release prebuilt | SUCCESS (cache hit) | FAIL — `ENOTFOUND github.com`, source-build fallback also fails | SUCCESS (prebuilt download) |
| bcrypt@5.1.1 | `node-pre-gyp install --fallback-to-build`, GitHub-release prebuilt | SUCCESS | **SUCCESS** — prebuilt download net-blocked, but `--fallback-to-build` compiled from source OFFLINE (warm node-gyp headers) | SUCCESS |
| @swc/core@1.7.26 | postinstall `node postinstall.js`, pure optionalDep | SUCCESS | SUCCESS — postinstall NEVER touches the network (validates the binding, else wasm fallback) | n/a |

Reading the matrix against the thesis:

- **esbuild, better-sqlite3 — clean fit.** Warm jailed succeeds offline from cache;
  cold jailed fails on the network step; cold succeeds the instant net is lifted.
  Net is the sole blocker, so prefetch (warming the cache) is the entire fix.
- **bcrypt — fits, with a wrinkle.** Cold jailed still SUCCEEDS because
  `--fallback-to-build` compiles bcrypt from source offline (it needs no network
  when the node-gyp headers are already cached). The jail correctly let a
  legitimate from-source native build through. It is just not a clean "cold ⇒
  net failure" demo, because bcrypt doesn't actually NEED the network when it can
  build locally.
- **@swc/core — out of scope for the net thesis.** Its native binary arrives purely
  via npm `optionalDependencies`; the postinstall does no download. This whole
  class (esbuild's *binary*, swc, most `@napi-rs`/`napi` packages) is trivially
  jail-compatible — the "prefetch" is just npm resolving the platform package,
  which already happens outside the jail.

## Design-relevant findings

1. **node-gyp's macOS devdir is outside the write set (real gap).**
   `script_sandbox::default_extra_write` grants write to `~/.cache/node-gyp`
   (the Linux/XDG path), but node-gyp on macOS uses `~/Library/Caches/node-gyp`.
   Probed under the jail: `~/.cache/node-gyp` ⇒ writable; `~/Library/Caches/node-gyp`
   ⇒ `Operation not permitted`. A from-source build worked in this matrix ONLY
   because the headers were already present and reads are generous
   (read-deny-set aside). On a COLD header cache the build would be blocked twice
   over: the header download (nodejs.org) is net-denied, AND even a prefetch that
   warms the headers can't write them to the denied macOS path. **Action:**
   `default_extra_write` must include the per-OS node-gyp devdir
   (`~/Library/Caches/node-gyp` on macOS), and prefetch must warm that path — not
   only `~/.cache/node-gyp`.

2. **esbuild's `bin/esbuild` self-optimization fails closed (benign).** On first
   install esbuild best-effort hard-links the native binary over its JS launcher;
   under the jail that optimization silently fails (the `try/catch` swallows it)
   and the slower JS-launcher path is kept. No functional breakage. Re-running
   `install.js` on an ALREADY-optimized tree under the jail DOES crash — but that
   is not a real install path (the lifecycle runs once, on the JS-launcher tree).

3. **Coarse net-deny behaves exactly as the policy claims.** `net.enforce` ⇒
   Seatbelt `(deny network*)` blocks all egress including loopback and AF_UNIX;
   the `allow_hosts` list is currently inert (proxy unwired), so the effective
   default is net-deny-all — which is what prefetch-primary wants. The honest
   `Degradation { lost: ["net-per-host"], reason: "egress proxy not yet wired" }`
   is surfaced.

## Attack containment (all blocked jailed; all negative-controlled)

Run via `examples/jail_run.rs` against a planted fake HOME. Each attack is also
run with the relevant axis lifted, confirming the test isn't hollow.

| attack | jailed | control (axis lifted / unjailed) |
| --- | --- | --- |
| network exfil (`/dev/tcp` connect) | BLOCKED | CONNECTED (`--no-net-enforce`) |
| read `~/.ssh`, `~/.aws`, project `.env` | BLOCKED | READ (unjailed); still BLOCKED with net lifted ⇒ it's the fs read-deny axis |
| write outside package dir (`$HOME/.evil`) | BLOCKED | WROTE (unjailed) |
| read scrubbed secret env (`AWS_SECRET_ACCESS_KEY`, `NPM_TOKEN`) | SCRUBBED | PRESENT (`--no-env-scrub`) |

## Caveats

- macOS Seatbelt only. Linux (Landlock + seccomp) and Windows are UNTESTED here.
- The real-package runs use the real `~/.npm/_prebuilds` and node-gyp header
  caches for warmth, and the secret-read attack uses a planted fake HOME so no
  real credential is touched.
