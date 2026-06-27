# macOS write-confine probes — results

Ground-truth validation of the macOS Seatbelt (SBPL) **write-confine** mechanism nub's
build-jail relies on, run via `sandbox-exec` on macOS 15 (Darwin 25.5.0, arm64). Each finding
carries the exact command + captured output so a verdict cannot be vacuous. The harness lives
alongside this file (`gen-profile.mjs`, `jail-run.sh`); see `README.md` for the loop. The
canonical research synthesis is `wiki/research/macos-seatbelt-write-confine.md`.

## Verdicts

| Axis | Verdict | Evidence |
|---|---|---|
| Write-confine (pkg dir only) | ENFORCED | write inside pkg OK; write outside → EPERM |
| Symlink-escape bypass | BLOCKED | write through pkg-internal symlink to outside → EPERM |
| `..`-traversal bypass | BLOCKED | `pkg/../secret/x` → EPERM, file not created |
| Hardlink-creation-in-jail bypass | BLOCKED | `ln <outside> pkg/h` → EPERM at creation |
| Firmlink canonicalization (`/tmp`,`/var`) | HANDLED | symlink-form write-allow is INERT; canonical-form allow works |
| Device-write minimum | tight literal set sufficient | `/dev/null` write needs a grant; literal set < `(subpath "/dev")` |
| node-gyp from-source (the headline) | strict BREAKS, relaxed BUILDS | devdir `mkdir` EPERM under strict; clean `.node` under relaxed+tmp-repoint |

## §Bypass — the bypass surface (exact probes)

Profile under test (write-allow = `<B>/pkg` + `/dev`, deny everything else):

**1. Symlink escape — BLOCKED.** `ln -sf <B>/secret <B>/pkg/link`, then inside the jail
`echo PWNED > <B>/pkg/link/data.txt`:
```
/bin/sh: <B>/pkg/link/data.txt: Operation not permitted
secret now: ORIGINAL                       # unchanged
```
Seatbelt resolves the symlink and checks the canonical target (`<B>/secret/...`), outside the
allow-set.

**2. `..` traversal — BLOCKED.** `echo PWNED > <B>/pkg/../secret/dd.txt`:
```
/bin/sh: <B>/pkg/../secret/dd.txt: Operation not permitted
ls: <B>/secret/dd.txt: No such file or directory     # not created
```

**3. Hardlink creation in-jail — BLOCKED.** A confined script cannot create the escaping link:
```
ln <B>/secret/data.txt <B>/pkg/h.txt
ln: <B>/pkg/h.txt: Operation not permitted           # creation denied
```
Creating a hardlink requires `file-write*` on the TARGET inode (outside the writable root) →
denied. (`(deny file-link*)` is NOT a valid Seatbelt op — parse error — and is unnecessary.)
Residual: a PRE-EXISTING hardlink inside pkg pointing out-of-root *is* writable, but the
confined script can't create one and a tarball can't deliver one (extractor rejects escaping
links; absolute victim paths aren't expressible in a tarball) — outside the threat model.

## §Canon — firmlink canonicalization (the load-bearing rule)

A write-allow given in **symlink form is inert** — the write is silently DENIED, because the
kernel checks the canonical path. Proven with a `mktemp -d` dir (`/var/folders/...` →
`/private/var/folders/...`):
```
# allow ONLY the /var/folders (symlink) form:
(allow file-write* (subpath "/var/folders/qg/.../T/tmp.X"))
echo hi > /var/folders/qg/.../T/tmp.X/viasym.txt   →  Operation not permitted   (DENIED)

# allow the canonical /private/var form:
(allow file-write* (subpath "/private/var/folders/qg/.../T/tmp.X"))
echo hi > /var/folders/qg/.../T/tmp.X/viacanon.txt →  WROTE_ALLOWING_CANON_FORM  (OK)
```
The same bit me with `/tmp` → `/private/tmp`. **Conclusion: the generator MUST canonicalize
every write-allow path.** `gen-profile.mjs` does (`canonicalizeForAllow`, unit-tested in
`gen-profile.test.mjs` — `node gen-profile.test.mjs` → ALL PASS).

## §Device — device-write minimum

Under a no-`/dev` write profile, `echo x > /dev/null` → `Operation not permitted` (build dies).
A tight literal set — `/dev/null /dev/zero /dev/tty /dev/dtracehelper /dev/random /dev/urandom
/dev/stdout /dev/stderr /dev/fd` — restores it, smaller than the reference's `(subpath "/dev")`.
Writing a real disk-device node needs root the jailed user lacks, so the subpath grant buys
nothing. Generator default = the literal set; `--dev-subpath` is the escape hatch.

## §node-gyp — the headline breakage + fix (better-sqlite3, from-source)

**STRICT (pkg + tmp only):** node-gyp's devdir write is DENIED:
```
gyp ERR! stack Error: EPERM: operation not permitted, mkdir '<fresh-devdir>'
```
Granting `--write <devdir>` alone is NOT enough — node-gyp also `mkdtemp`s in `os.tmpdir()` =
the OS temp ROOT:
```
gyp ERR! stack Error: EPERM: operation not permitted, mkdtemp '/var/folders/.../T/node-gyp-tmp-XXX'
```
**Fix = the `tmp: private` shorthand:** repoint `TMPDIR`/`TMP`/`TEMP` at the granted private
scratch (jail-run does this). With devdir granted + tmp repointed:
```
  CXX(target) Release/obj.target/better_sqlite3/src/better_sqlite3.o
  SOLINK_MODULE(target) Release/better_sqlite3.node
gyp info ok
=> better_sqlite3.node (1.9 MB) produced; devdir populated with headers
```
So the write set a from-source native build needs: **pkg dir + private scratch (tmp anchors
repointed) + the node-gyp devdir cache** (`~/Library/Caches/node-gyp/<ver>` on macOS by default).

## §Darwin — the Apple-toolchain confstr scratch (a macOS-specific residual)

Even with `TMPDIR`/`TMP`/`TEMP` repointed, a from-source compile emits:
```
make: error: couldn't create cache file '/var/folders/qg/.../T/xcrun_db-XXX' (errno=Operation not permitted)
cc:  error: couldn't create cache file '/var/folders/qg/.../T/xcrun_db-XXX' (errno=Operation not permitted)
```
`xcrun`/cc/libtool write their `xcrun_db` to the per-user **DARWIN_USER_TEMP_DIR**
(`/var/folders/<uid>/T`, canonical `/private/var/folders/<uid>/T`), resolved via confstr
(`_CS_DARWIN_USER_TEMP_DIR`) — **NOT redirectable by the `TMPDIR` env var**. Here it is
non-fatal (xcrun degrades), but it spams EPERM and could break a stricter toolchain. Granting
the confstr pair (`DARWIN_USER_TEMP_DIR` + `DARWIN_USER_CACHE_DIR`) silences it completely
(`gyp info ok`, zero xcrun noise). These are per-user OS scratch dirs (low risk). The generator
grants them with `--darwin-temp` (jail-run passes it by default); paths queried from `getconf`
so the `<uid>` hash is correct on any machine.

## §Collapse — base-anchor HOME-repoint (the optimization verdict)

**Repointing `HOME` at ONE granted cache root collapses the N per-tool caches to a single grant.**
Proven: relaxed jail granting ONLY `$CACHEROOT` (one root), with `HOME=$CACHEROOT` and the
DEFAULT node-gyp devdir (HOME-derived `$HOME/Library/Caches/node-gyp`):
```
gyp info ok
=> better_sqlite3.node produced; cache landed at $CACHEROOT/Library/Caches/node-gyp/26.2.0
```
node-gyp's devdir followed `HOME` into the single granted root — one grant covered it. (A
control with `--mode strict`, where `--write` is correctly ignored, denied `mkdir
$CACHEROOT/Library` — confirming strict gates the cache allowlist out, AND that the cache *did*
derive from HOME.) Per [`native-build-cache-paths`](../../wiki/research/native-build-cache-paths.md)
the same `os.homedir()` anchor moves 9/9 cache-bearing tools on macOS (where `~/Library/Caches`
has no env override), so the explicit N-dir cache enumeration collapses to: **pkg dir + private
scratch + ONE base-anchor cache root (HOME-repointed) + the DARWIN confstr pair.**

## §Catalog — cache-family holes (empirical sweep)

<!-- Filled from the cache-family empirical sweep: per family — representative package, install
command, exact denied out-of-package write path under strict, genuine-write-vs-prefetchable,
HOME-repoint capture. -->

## Conclusion (so far)

The write-confine mechanism is sound and bypass-resistant on macOS: symlink-escape, `..`, and
in-jail hardlink-creation all fail closed because Seatbelt matches the canonical path and gates
link-creation on the target. The one mandatory correctness rule is **canonicalizing every
write-allow path** (a symlink-form allow is inert → would deny all writes and break every build
under a `/tmp`- or `/var`-rooted tree). Package-dir-only is not a viable write set; pkg + private
tmp + one base-anchor-captured cache root is. The cache-family catalog + the HOME-collapse
verdict follow below once the sweep completes.
