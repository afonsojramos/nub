// Blob-URL worker source capture — the SYNC half of WHATWG `blob:` worker support.
//
// A `new Worker(blobUrl)` must read the Blob's source SYNCHRONOUSLY in the
// constructor, but a Blob's bytes are only readable via the async Blob.text /
// arrayBuffer. We close that gap by snapshotting the source at
// `URL.createObjectURL(blob)` time — which always runs BEFORE the Worker is
// constructed — into a registry keyed by the minted URL.
//
// This lives in its OWN tiny CJS module (no node:worker_threads dependency) so the
// main-thread preload can install the wrap EAGERLY: it must be live before user
// code calls createObjectURL, whereas worker-polyfill.mjs (which pulls
// worker_threads + the whole streams/worker-io builtin set) is loaded LAZILY on
// first `new Worker` to protect cold start. The Worker class imports THIS module to
// read `blobUrlSources`. Touches only URL / Blob / Buffer — all already-realized
// core globals — so requiring it adds nothing to the main-thread bootstrap set.

// blob: URL → source text. Shared with worker-polyfill.mjs's Worker constructor.
const blobUrlSources = new Map();
// Blob construction parts, remembered so createObjectURL can assemble source sync.
const blobParts = new WeakMap();

function decode(parts) {
  let src = "";
  for (const p of parts ?? []) {
    if (typeof p === "string") src += p;
    else if (typeof Buffer !== "undefined" && Buffer.isBuffer(p)) src += p.toString("utf8");
    else if (ArrayBuffer.isView(p)) src += Buffer.from(p.buffer, p.byteOffset, p.byteLength).toString("utf8");
    else if (p instanceof ArrayBuffer) src += Buffer.from(p).toString("utf8");
    else if (typeof p === "object" && p && typeof p.size === "number") {
      const nested = blobParts.get(p); // a nested Blob made via our wrapper
      if (nested) src += decode(nested);
    }
  }
  return src;
}

// Wrap URL.createObjectURL/revokeObjectURL and subclass Blob to remember parts.
// Idempotent + transparent for every non-worker use. No-op when URL has no
// createObjectURL (older floors) — blob: workers are then simply unavailable.
function installBlobUrlSupport() {
  if (typeof URL === "undefined" || typeof URL.createObjectURL !== "function") return;
  if (URL.createObjectURL.__nubWrapped) return;

  const NativeBlob = globalThis.Blob;
  if (typeof NativeBlob === "function" && !NativeBlob.__nubWrapped) {
    // Transparent subclass: `instanceof Blob` stays true and the full Blob API is
    // intact; we only record the construction parts for sync source assembly.
    class Blob extends NativeBlob {
      constructor(parts = [], options) {
        super(parts, options);
        blobParts.set(this, parts);
      }
    }
    Object.defineProperty(Blob, "__nubWrapped", { value: true });
    Object.defineProperty(globalThis, "Blob", {
      value: Blob,
      enumerable: false,
      writable: true,
      configurable: true,
    });
  }

  const nativeCreate = URL.createObjectURL.bind(URL);
  const nativeRevoke =
    typeof URL.revokeObjectURL === "function" ? URL.revokeObjectURL.bind(URL) : null;
  const wrappedCreate = function createObjectURL(obj) {
    const url = nativeCreate(obj);
    const parts = blobParts.get(obj);
    if (parts) blobUrlSources.set(url, decode(parts));
    return url;
  };
  Object.defineProperty(wrappedCreate, "__nubWrapped", { value: true });
  URL.createObjectURL = wrappedCreate;
  if (nativeRevoke) {
    URL.revokeObjectURL = function revokeObjectURL(url) {
      blobUrlSources.delete(url);
      return nativeRevoke(url);
    };
  }
}

module.exports = { blobUrlSources, installBlobUrlSupport };
