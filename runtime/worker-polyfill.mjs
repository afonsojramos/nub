// Browser-shape Worker global polyfill for Node.js.
// Wraps node:worker_threads.Worker with EventTarget inheritance,
// real MessageEvent/ErrorEvent, and URL-only constructor (Deno shape).

// node: builtins are fetched via `process.getBuiltinModule` rather than static
// `import`. This file is loaded via `require(esm)` from the preload (polyfills.cjs),
// and Node's `require(esm)` instantiates an ES module by walking its STATIC IMPORT
// graph through whatever ESM loader chain is registered — including the USER's
// `--loader`/`register()` hooks. A static `import { Worker } from
// "node:worker_threads"` therefore routes the builtin through the user chain; a
// user load hook that returns SOURCE for node:worker_threads makes V8 see no
// `Worker` export, so `new NodeWorker(...)` references an undefined binding and
// the child crashes (observed against test-esm-loader-chaining). `process
// .getBuiltinModule` fetches the real builtin synchronously off the loader graph,
// bypassing the user chain entirely — same fix transform-core.mjs uses.
//
// The bootstrap MUST avoid a static `node:module` import too: it has the IDENTICAL
// leak. The chaining corpus registers a user load hook (loader-load-foo-or-42.mjs)
// that rewrites the SOURCE of `node:module` so its compiled namespace no longer
// exports `createRequire` — so a static `import { createRequire } from "node:module"`
// here threw `does not provide an export named 'createRequire'` and crashed every
// run with that loader (the earlier comment claimed user hooks "don't intercept
// node:module" — FALSE; this is the bug). So we use `process.getBuiltinModule` when
// present (fast tier + modern compat: no static import, nothing for the user chain
// to observe), and on the narrow FLOOR where it's absent (Node < 22.3/20.16/18.20.4,
// loaded only via the compat-tier entries OFF any user chain) the createRequire
// THREADED IN through `setBootstrapCreateRequire` below.
//
// BRAND BOUNDARY — the floor's `createRequire` is threaded through MODULE SCOPE, never
// parked on `globalThis` (a `globalThis.__nub*` sentinel is the same brand leak as a
// NUB_* env var — enumerable in user code AND worker realms — so it is forbidden). On
// the floor this module is loaded ONLY via the compat-tier main-thread preload
// (preload.mjs), which imports floor-builtin first, then — AFTER importing this module
// — calls `setBootstrapCreateRequire(createRequire)` and `installWorkerPolyfill()`. So
// the install work is deferred (this module does NOT auto-run on the floor): its body
// fetches builtins, and on the floor those aren't reachable until the setter runs.
// On the fast tier (getBuiltinModule present) the install runs eagerly at module eval
// — see the auto-install at the bottom — so the existing side-effect-`require` call
// sites (preload.cjs, polyfills.cjs) are unchanged.
let _bootstrapCreateRequire = null;
export function setBootstrapCreateRequire(fn) {
  _bootstrapCreateRequire = fn;
}
function __getBuiltin(id) {
  if (typeof process.getBuiltinModule === "function") return process.getBuiltinModule(id);
  return _bootstrapCreateRequire(import.meta.url)(id);
}

// `ErrorEvent` only became a global in Node 26. On the 22/24 floor it is
// undefined, so `new ErrorEvent(...)` below would throw a ReferenceError inside
// the worker's "error" handler — crashing the PARENT thread on every worker
// that throws. Resolve the constructor lazily and memoize on first use: use the
// native global when present, otherwise a minimal Event subclass carrying the
// standard ErrorEvent fields (message/error/filename/lineno/colno).
// See wiki/research/worker-polyfill.md.
//
// LAZY (not resolved at module load) on purpose: reading `globalThis.ErrorEvent`
// at top level trips Node's lazy `ErrorEvent` getter, which eagerly realizes
// ~100+ builtins (http2/tls/crypto/zlib/perf_hooks/webstreams) on EVERY startup
// — a cold-start regression (process.moduleLoadList ~230 vs node's ~110) that
// contradicts nub's fast-runner premise for the common "run a plain file, never
// touch Workers" case. The constructors are only ever needed inside the
// post-construction error/message handlers, so deferring resolution there costs
// nothing for non-Worker programs and nothing measurable for Worker ones.
let _ErrorEventCtor;
function getErrorEventCtor() {
  return (_ErrorEventCtor ??=
    typeof globalThis.ErrorEvent === "function"
      ? globalThis.ErrorEvent
      : class ErrorEvent extends Event {
          constructor(type, init = {}) {
            super(type, init);
            this.message = init.message ?? "";
            this.error = init.error ?? null;
            this.filename = init.filename ?? "";
            this.lineno = init.lineno ?? 0;
            this.colno = init.colno ?? 0;
          }
        });
}

// Define the browser-shape `Worker` global (main thread) + the worker-side scope
// (self/postMessage/message wiring). Acquires its node: builtins on entry — on the
// floor that needs the threaded createRequire, so this runs only after the compat
// entry has called setBootstrapCreateRequire (or, on the fast tier, eagerly via the
// auto-install at the bottom).
export function installWorkerPolyfill() {
  const { Worker: NodeWorker, parentPort, isMainThread } = __getBuiltin("node:worker_threads");
  const { fileURLToPath } = __getBuiltin("node:url");
  // blob: worker source registry, shared with the eager main-thread preload that
  // wraps URL.createObjectURL (worker-blob-url.cjs). Loaded via createRequire so
  // both this lazily-loaded ESM module and the eager CJS preload reference the SAME
  // module instance (Node dedupes by resolved path) — i.e. the SAME blobUrlSources.
  const { blobUrlSources, installBlobUrlSupport } = (
    typeof process.getBuiltinModule === "function"
      ? __getBuiltin("node:module").createRequire(import.meta.url)
      : _bootstrapCreateRequire(import.meta.url)
  )("./worker-blob-url.cjs");

  // Resolve a worker-error stack frame to {filename,lineno,colno} so the
  // ErrorEvent carries real source location, per WHATWG §10.2.6 (the spec
  // requires these fields populated from where the error was raised). Node's
  // `error` event delivers the thrown Error; we read its first stack frame.
  // Browser-scrubbing of cross-origin frames does not apply here (all worker
  // sources are same-origin local), so we surface the raw location.
  //
  // The frame is anchored at `at ` and consumes the optional `Func (` wrapper so
  // group 1 is JUST the path (a `file://` URL, an absolute POSIX path, or a
  // Windows `C:\…` path — the leading `C:` is NOT mistaken for the location
  // colon because the line:col are the LAST two `:`-segments). Without the anchor
  // + wrapper-consumption the path captured the `at Func (` prefix verbatim.
  const STACK_FRAME = /^at\s+(?:.+?\s+\()?(.+?):(\d+):(\d+)\)?$/;
  function locationFromError(err) {
    let filename = "";
    let lineno = 0;
    let colno = 0;
    const stack = err && typeof err.stack === "string" ? err.stack : "";
    for (const line of stack.split("\n")) {
      const t = line.trim();
      if (!t.startsWith("at ")) continue;
      const m = STACK_FRAME.exec(t);
      if (m) {
        filename = m[1].startsWith("file://") ? fileURLToPath(m[1]) : m[1];
        lineno = Number(m[2]) || 0;
        colno = Number(m[3]) || 0;
        break;
      }
    }
    return { filename, lineno, colno };
  }

  if (typeof globalThis.Worker === "undefined") {
  class Worker extends EventTarget {
    #worker;
    #name;

    constructor(url, options = {}) {
      super();

      // The WHATWG Worker constructor accepts a script URL. Per §10.2.6.3 the
      // standard inline mechanisms are `blob:` and `data:` URLs (there is no
      // inline-source-string form in the spec). We map each to a Node spawn:
      //   - file path / file: URL  → spawn the file (transpiled by nub's preload)
      //   - data: URL              → Node runs it directly (worker_threads v14.9)
      //   - blob: URL              → resolve the Blob via node:buffer, spawn its
      //                              source with eval:true (Node can't open blob:)
      let spawnTarget;
      let evalMode = false;

      const asUrlString =
        url instanceof URL ? url.href : typeof url === "string" ? url : null;
      if (asUrlString === null) {
        throw new TypeError("Worker constructor: url must be a string or URL");
      }

      if (asUrlString.startsWith("blob:")) {
        // A `blob:` worker (WHATWG inline mechanism). Node cannot open a blob:
        // URL as a worker entry, and the Blob's bytes are only readable
        // ASYNCHRONOUSLY (Blob.text/arrayBuffer) while this constructor is sync.
        // We close that gap by snapshotting the source SYNCHRONOUSLY at
        // `URL.createObjectURL(blob)` time (see installBlobUrlSupport below) into
        // a module-scope registry keyed by URL, then spawn it with eval:true.
        const source = blobUrlSources.get(asUrlString);
        if (source === undefined) {
          throw new TypeError(
            `Worker constructor: blob URL '${asUrlString}' is not a known object URL`
          );
        }
        spawnTarget = source;
        evalMode = true;
      } else if (asUrlString.startsWith("data:")) {
        spawnTarget = new URL(asUrlString);
      } else if (asUrlString.startsWith("file://")) {
        spawnTarget = fileURLToPath(asUrlString);
      } else {
        spawnTarget = asUrlString;
      }

      this.#name = typeof options.name === "string" ? options.name : "";

      // `type: "module" | "classic"` selects the worker's module system per the
      // spec. The worker-side scope exposes `importScripts` only for classic
      // workers (WHATWG WorkerGlobalScope — classic-only) and throws for module
      // workers. nub signals the choice to the worker via the internal
      // NUB_WORKER_TYPE env (internal plumbing var — exempt from the brand
      // boundary). Node still decides the entry's actual module/CJS PARSING by
      // file extension + package.json "type"; this env governs only which
      // importScripts surface the polyfill installs.
      const workerType =
        options.type === "classic" ? "classic" : "module";

      // Node rejects flags in Worker execArgv that imply V8 `--harmony-*` staging
      // flags (ERR_WORKER_INVALID_EXEC_ARGV): `--harmony-*` themselves, and
      // `--experimental-shadow-realm` (implies `--harmony-shadow-realm`). Strip
      // those from whatever execArgv we forward.
      const stripHarmony = (argv) =>
        argv.filter(
          f => !f.startsWith("--harmony") && f !== "--experimental-shadow-realm"
        );
      // execArgv: forward nub's preload-carrying parent execArgv by DEFAULT (so a
      // worker inherits nub's transpile augmentation), but if the user supplied
      // their own execArgv, MERGE rather than clobber — parent flags first, user
      // flags appended so the user's win on conflict.
      const execArgv = stripHarmony(
        Array.isArray(options.execArgv)
          ? [...process.execArgv, ...options.execArgv]
          : process.execArgv
      );

      const nodeOptions = {
        ...options,
        eval: evalMode,
        execArgv,
      };
      // Signal the worker type via the internal NUB_WORKER_TYPE env without
      // disturbing the user's env semantics. `worker_threads.SHARE_ENV` is a
      // Symbol (live-shared parent env) — spreading it would destroy the share —
      // so in that case we leave env untouched and let the worker default
      // importScripts to the classic form (the only edge where the module-worker
      // throwing-importScripts nicety is skipped).
      const userEnv = options.env;
      if (typeof userEnv === "symbol") {
        // SHARE_ENV: leave nodeOptions.env as the user gave it; can't inject.
      } else {
        nodeOptions.env = { ...(userEnv ?? process.env), NUB_WORKER_TYPE: workerType };
      }
      if (this.#name) nodeOptions.name = this.#name;

      this.#worker = new NodeWorker(spawnTarget, nodeOptions);

      this.#worker.on("message", (data) => {
        this.dispatchEvent(new MessageEvent("message", { data }));
      });

      // WHATWG: messageerror fires when an inbound message fails deserialization;
      // it is a plain MessageEvent with `data: null` (NOT carrying the error).
      this.#worker.on("messageerror", () => {
        this.dispatchEvent(new MessageEvent("messageerror", { data: null }));
      });

      this.#worker.on("error", (err) => {
        const ErrorEventCtor = getErrorEventCtor();
        const { filename, lineno, colno } = locationFromError(err);
        this.dispatchEvent(
          new ErrorEventCtor("error", {
            error: err,
            message: err.message,
            filename,
            lineno,
            colno,
          })
        );
      });
      // No `exit` event: WHATWG Workers have no exit event (it is a
      // node:worker_threads concept, not part of the web Worker surface).
    }

    get name() {
      return this.#name;
    }

    postMessage(data, transfer) {
      this.#worker.postMessage(data, transfer);
    }

    // WHATWG terminate() returns void. Node's returns a Promise; we discard it so
    // the surface matches the spec (the underlying termination still proceeds).
    terminate() {
      this.#worker.terminate();
    }

    #onmessageHandler = null;
    get onmessage() { return this.#onmessageHandler; }
    set onmessage(fn) {
      if (this.#onmessageHandler) this.removeEventListener("message", this.#onmessageHandler);
      this.#onmessageHandler = fn;
      if (fn) this.addEventListener("message", fn);
    }

    #onerrorHandler = null;
    get onerror() { return this.#onerrorHandler; }
    set onerror(fn) {
      if (this.#onerrorHandler) this.removeEventListener("error", this.#onerrorHandler);
      this.#onerrorHandler = fn;
      if (fn) this.addEventListener("error", fn);
    }

    #onmessageerrorHandler = null;
    get onmessageerror() { return this.#onmessageerrorHandler; }
    set onmessageerror(fn) {
      if (this.#onmessageerrorHandler) this.removeEventListener("messageerror", this.#onmessageerrorHandler);
      this.#onmessageerrorHandler = fn;
      if (fn) this.addEventListener("messageerror", fn);
    }
  }

  // NON-ENUMERABLE: invisible to `Object.keys(globalThis)` / for-in is the
  // additive contract — vanilla-Node code that enumerates the global object must
  // not observe nub's injected `Worker`. Node defines its own globals the same
  // way. Writable+configurable so user code can still override it.
  Object.defineProperty(globalThis, "Worker", {
    value: Worker,
    enumerable: false,
    writable: true,
    configurable: true,
  });

  // Enable blob: workers: wrap URL.createObjectURL so the source is captured
  // synchronously for the constructor's blob: branch. Transparent for all other
  // uses; installs once. Only on the main thread (where blob: URLs are minted).
  if (isMainThread) installBlobUrlSupport();
}

// Worker-side bootstrap: emulate the DedicatedWorkerGlobalScope on top of
// node:worker_threads — `self`, `postMessage`, `close`, AND inbound message
// events. Node's worker global is not an EventTarget and exposes none of these
// (verified), so the polyfill provides the whole surface. Without the inbound
// wiring, `self.onmessage` / `self.addEventListener("message", …)` never fire
// and a parent→worker round-trip hangs — see wiki/research/worker-polyfill.md.
if (!isMainThread && parentPort) {
  const scope = globalThis;
  // All of nub's worker-scope global injections below (self, addEventListener,
  // removeEventListener, dispatchEvent, postMessage, close) are defined
  // NON-ENUMERABLE. Node's worker global is not an EventTarget and exposes none
  // of these, so a worker doing `Object.keys(globalThis)` / for-in must not see
  // nub's additions — invisibility-to-enumeration is the additive contract.
  // writable+configurable mirrors Node's own global descriptors. (`onmessage`/
  // `onmessageerror` below already use Object.defineProperty, whose enumerable
  // defaults to false.)
  const defineGlobal = (name, value) =>
    Object.defineProperty(scope, name, {
      value,
      enumerable: false,
      writable: true,
      configurable: true,
    });
  defineGlobal("self", scope);

  // `self.name` — the worker's name from the constructor's {name} option (WHATWG
  // DedicatedWorkerGlobalScope.name). Node backs the {name} ctor option with a
  // per-thread `worker_threads.threadName` (v18.16, safe at nub's 18.19 floor),
  // readable inside the worker — so we surface that directly, no extra plumbing.
  {
    const wt = __getBuiltin("node:worker_threads");
    const name = wt && typeof wt.threadName === "string" ? wt.threadName : "";
    Object.defineProperty(scope, "name", {
      value: name,
      enumerable: false,
      writable: true,
      configurable: true,
    });
  }

  // `importScripts(...urls)` — WHATWG WorkerGlobalScope, CLASSIC workers only.
  // Synchronously fetches + evaluates each script in the global scope, in order.
  // A module worker MUST throw on importScripts (use `import` instead). nub learns
  // the worker's type from NUB_WORKER_TYPE (set by the main-side constructor).
  // Remote URLs are not supported (no sync network in Node); local file:/relative
  // paths and data: URLs are read synchronously.
  // Default to the classic (working) importScripts when the type is unset — this
  // is the SHARE_ENV edge where the constructor couldn't inject NUB_WORKER_TYPE.
  if (process.env.NUB_WORKER_TYPE !== "module") {
    const fs = __getBuiltin("node:fs");
    const { fileURLToPath: f2p, pathToFileURL } = __getBuiltin("node:url");
    defineGlobal("importScripts", (...urls) => {
      for (const u of urls) {
        const s = String(u);
        let code;
        if (s.startsWith("data:")) {
          const comma = s.indexOf(",");
          const meta = s.slice(5, comma);
          const body = s.slice(comma + 1);
          code = meta.includes("base64")
            ? Buffer.from(body, "base64").toString("utf8")
            : decodeURIComponent(body);
        } else if (/^https?:/.test(s)) {
          throw new TypeError(
            "importScripts: remote URLs are not supported (no synchronous network)"
          );
        } else {
          const path = s.startsWith("file://") ? f2p(s) : s;
          code = fs.readFileSync(path, "utf8");
        }
        // Indirect eval → runs in global scope, matching importScripts semantics.
        (0, eval)(code);
      }
    });
  } else {
    // Module workers: importScripts must throw (spec). Provide the throwing form
    // so the surface exists and the error is the spec-correct one.
    defineGlobal("importScripts", () => {
      throw new TypeError("importScripts is not available in module workers");
    });
  }

  // `message`/`messageerror` are DELEGATED straight onto the native `parentPort`
  // (a real Node MessagePort) so Node's own C++ event-loop ref-counting governs
  // worker lifetime: a worker that never listens leaves parentPort with no
  // listeners → Node unrefs it → the worker exits naturally (matching
  // `node:worker_threads` and Bun); a worker listening via `self.onmessage` /
  // `addEventListener("message", …)` refs it → stays alive. Node reflects
  // `{once}`/`{signal}`/last-listener removal in the loop ref-count in C++,
  // which no userland counter can observe. (Earlier this block eagerly held a
  // `parentPort.on("message")` forwarder, which kept EVERY worker's event loop
  // alive → pure `parentPort` workers that should exit hung forever. See
  // wiki/research/worker-polyfill.md §4.) All OTHER event types go to a private
  // EventTarget (additive; no globalThis prototype re-parenting — `event.target`
  // is that private target, the documented minor divergence).
  const other = new EventTarget();
  const otherAdd =
    typeof scope.addEventListener === "function"
      ? scope.addEventListener.bind(scope)
      : other.addEventListener.bind(other);
  const otherRemove =
    typeof scope.removeEventListener === "function"
      ? scope.removeEventListener.bind(scope)
      : other.removeEventListener.bind(other);
  const otherDispatch =
    typeof scope.dispatchEvent === "function"
      ? scope.dispatchEvent.bind(scope)
      : other.dispatchEvent.bind(other);

  const DELEGATED = new Set(["message", "messageerror"]);
  // user listener → its parentPort wrapper (per delegated event), so
  // removeEventListener detaches the exact wrapper Node registered.
  const wrappers = { message: new Map(), messageerror: new Map() };

  function addDelegated(evt, listener, opts) {
    const cb =
      typeof listener === "function"
        ? listener
        : listener && typeof listener.handleEvent === "function"
          ? (e) => listener.handleEvent(e)
          : null;
    if (!cb) return;
    const map = wrappers[evt];
    if (map.has(listener)) return; // EventTarget dedups identical (type, listener)
    const o = opts && typeof opts === "object" ? opts : {};
    if (o.signal && o.signal.aborted) return;
    const fire = (data) => cb.call(scope, new MessageEvent(evt, { data }));
    let wrapper;
    if (o.once) {
      wrapper = (data) => {
        map.delete(listener);
        fire(data);
      };
      parentPort.once(evt, wrapper);
    } else {
      wrapper = fire;
      parentPort.on(evt, wrapper);
    }
    map.set(listener, wrapper);
    if (o.signal) {
      o.signal.addEventListener("abort", () => removeDelegated(evt, listener), {
        once: true,
      });
    }
  }
  function removeDelegated(evt, listener) {
    const wrapper = wrappers[evt].get(listener);
    if (wrapper) {
      parentPort.off(evt, wrapper);
      wrappers[evt].delete(listener);
    }
  }

  defineGlobal("addEventListener", (type, listener, opts) =>
    DELEGATED.has(type)
      ? addDelegated(type, listener, opts)
      : otherAdd(type, listener, opts));
  defineGlobal("removeEventListener", (type, listener, opts) =>
    DELEGATED.has(type)
      ? removeDelegated(type, listener)
      : otherRemove(type, listener, opts));
  defineGlobal("dispatchEvent", (ev) => otherDispatch(ev));

  // `onmessage` / `onmessageerror` register via the delegating add/remove above,
  // mirroring the web API and the main-side Worker. Assigning `null` removes the
  // last listener → parentPort unrefs → the worker can exit (Bun parity).
  for (const evt of ["message", "messageerror"]) {
    let handler = null;
    Object.defineProperty(scope, "on" + evt, {
      configurable: true,
      get() {
        return handler;
      },
      set(fn) {
        if (handler) scope.removeEventListener(evt, handler);
        handler = typeof fn === "function" ? fn : null;
        if (handler) scope.addEventListener(evt, handler);
      },
    });
  }

  // Outbound + lifecycle.
  if (typeof scope.postMessage !== "function") {
    defineGlobal("postMessage", (data, transfer) => parentPort.postMessage(data, transfer));
  }
  if (typeof scope.close !== "function") {
    defineGlobal("close", () => process.exit(0));
  }
  }
}

// Fast tier (and modern compat): getBuiltinModule is present, so the install needs no
// threaded createRequire — run it eagerly at module eval, preserving the side-effect-
// on-`require` contract the fast-tier call sites (preload.cjs, polyfills.cjs) rely on.
// On the FLOOR (getBuiltinModule absent) this is skipped; the compat main-thread
// preload calls setBootstrapCreateRequire(...) + installWorkerPolyfill() explicitly.
if (typeof process.getBuiltinModule === "function") installWorkerPolyfill();
