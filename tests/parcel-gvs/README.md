# Parcel under the global virtual store — regression harness

This directory regression-tests a global-virtual-store (GVS) bug that broke `parcel build`: the shared store materialized `@parcel/core` as two byte-identical directories, so the main thread and a worker thread loaded different `@parcel/core` instances, ended up with two module-scoped serializer registries, and threw `DataCloneError` at worker-farm startup.

## The bug

The resolver widens its graph with every common platform's optional native deps so the committed lockfile is portable. The link phase runs `filter_graph` to trim that back to the host before hashing. The GVS prewarm materializer — which populates the shared store concurrently with fetch — hashed the **widened** graph instead. Any package whose subtree contains a platform-specific optional native dep (all of Parcel's tree: `@parcel/watcher-<platform>`, `@swc/core-<platform>`, `lmdb`, `msgpackr`) then hashed differently in the two phases, so the same `dep_path` landed at two shared-store directories. The existence-gated link step never rewired over the prewarm cohort, so `parcel` resolved one copy of `@parcel/core` and `@parcel/workers` resolved the other.

The fix host-filters the prewarm graph to match the link phase (`run_gvs_prewarm_materializer` in `vendor/aube/crates/aube/src/commands/install/materialize.rs`). A hermetic unit test guards the graph-hash agreement invariant: `gvs_prewarm_and_link_agree_only_after_host_filtering_the_widened_graph` in `vendor/aube/crates/aube-resolver/src/platform.rs`.

## Running the harness

```sh
tests/parcel-gvs/run.sh target/fast/nub                      # default version matrix
tests/parcel-gvs/run.sh target/fast/nub 2.12.0 2.16.4        # specific versions
```

For each Parcel version, `run.sh` builds a minimal worker-farm fixture (`make-fixture.sh`), installs it into a **fresh, isolated** global store with the GVS forced on, and asserts exactly one `@parcel/core` store directory plus a `parcel build` that exits 0. Store isolation (`XDG_CACHE_HOME`/`XDG_DATA_HOME` per version) is load-bearing: a polluted machine-global store accumulates directories across installs and masks the over-split.

Verified against 2.9.3, 2.10.3, 2.11.0, 2.12.0, 2.13.3, and 2.16.4.

## Why an end-to-end harness

The runtime failure only reproduces against a real Parcel dependency tree materialized into a real shared store — the split is in on-disk store-directory naming and Parcel's own module-singleton assumption, neither of which a Rust unit test can stand in for. The unit test covers the hashing invariant; this harness covers the whole install-and-build path across Parcel versions.
