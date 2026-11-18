# 05 — SDK / world phases not built

**Decided (plan `hashed-hopping-lecun`, `docs/component-worlds/`).** std is expressed
through SDKs and worlds: runtime-provided like V8/Node intrinsics, no parser/AST hacking,
types known by declaration, host items bound by a per-runtime registry.

**Done.** Interface modules, opaque records, worlds, `impl interface`, digests and `#[since]`,
registry/installed table, provider `NativeBindings`, intrinsic/member contracts consumed by
effects and GC metadata.

**Not done.**
- Embedder-intrinsic reference harness and safe emitter API (forms: Cyallo body, declarative
  ops, guarded fast path, host call, embedder intrinsic with reference semantics and
  conformance harness).
- The load handshake and generated `W::Env` / `Exports` / `load`.
- The `prelude` SDK / init world as a bundle of capabilities (only a re-export module exists).
- Converting the std packages (`core`, `collections`, `tasks`, ...) into SDK packages.
- Updating `docs/component-worlds/` to the `core` exception (see 01).

**Decision needed.** Package split names (`core`, `collections`, `tasks`, others) and which
capabilities the init world bundles by default.
