// A minimal pass-through USER async ESM loader, the shape `@tailwindcss/node`
// registers (esm-cache.loader.mjs) when Tailwind v4 runs under Turbopack. Registering
// THIS via `module.register()` while nub's fast-tier sync `module.registerHooks` resolve
// hook is active is the whole trigger for the resolveSync() stub crash — see main.mjs.
export async function resolve(specifier, context, nextResolve) {
  return nextResolve(specifier, context);
}
