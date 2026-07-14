# Seasoned Rust design lens

Use this lens for Rust architecture, implementation, and whole-diff simplification. The target is idiomatic, direct, unsurprising code that looks maintained by an experienced Rust engineer: compact without cleverness.

## Ownership and lifecycle

- Let ownership and RAII model lifecycle. Prefer one clear owner and scoped cleanup over shared control flags or background cleanup.
- Keep data flow direct. Introduce a channel, callback, task, or thread only when the execution boundary requires it.
- Count `Arc<Mutex<_>>`, channels, threads or tasks, callbacks, global registries, background cleanup, and `unsafe` against the design's complexity budget. State why each is necessary.

## Types and abstractions

- Prefer ordinary enums, structs, and functions over a framework.
- Avoid a generic trait with one implementation, a builder with one construction path, overlapping state machines, and manager/controller/coordinator layers unless current requirements need the indirection.
- Make invalid states unrepresentable when that reduces states overall; do not replace two booleans with a larger ceremonial state machine.
- Use errors proportional to the public contract. Preserve useful context without creating an internal error taxonomy for unreachable distinctions.

## Platform boundaries

- Keep platform `cfg` branches cohesive and local to the platform operation rather than scattering conditional behavior across orchestration code.
- Localize `unsafe` behind the smallest safe interface. Document the invariant that makes it safe and test observable behavior.
- Prefer standard-library and kernel primitives whose lifecycle and failure semantics already match the requirement.

## Review and tests

- Make comments explain design decisions, safety invariants, and non-obvious platform constraints. Do not narrate the code.
- Test each behavior and security property once at the strongest useful boundary. Avoid multiplying tests for every internal transition or harness permutation.
- During whole-diff review, look for types, layers, flags, and cleanup paths that can be removed without weakening an approved invariant.
