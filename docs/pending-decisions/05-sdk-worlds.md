# 05 — SDK / world phases not built

**Decided (plan `hashed-hopping-lecun`, `docs/component-worlds/`).** std is expressed
through SDKs and worlds: runtime-provided like V8/Node intrinsics, no parser/AST hacking,
types known by declaration, host items bound by a per-runtime registry.

**Done.** Interface modules, opaque records, worlds, `impl interface`, digests and `#[since]`,
registry/installed table, declaration-linked `ResolvedBindings`, the provider's
`NativeBindings`, and intrinsic/member contracts used by effects and GC metadata.
[SDK terminology](../sdk-registration.md#terminology)

**Scalar execution fixture.** A verified interpreter fixture installs its
table, selects a checked Cyallo reference by declaration ID, and executes the
selected body through an alias. It exercises independent installations and
typed rejection of unsupported contracts and mismatched references.
[Fixture](../../tests/sdk_reference.rs)

**Synchronous host fixture.** A bodyless scalar procedure resolves through
the installed table to an embedder callback. One-instruction slices verify
exactly-once execution and typed failure at the call site.
[Host fixture](../../tests/sdk_host.rs)

**Not done.**
- Execute general bound calls using installed runtime tables, including
  parking, collecting and reentrant operations, opaque values and
  world-scoped dependencies.
- Embedder-intrinsic reference harness and safe emitter API (forms: Cyallo body, declarative
  ops, guarded fast path, host call, embedder intrinsic with reference semantics and
  conformance harness).
- The load handshake and generated `W::Env` / `Exports` / `load`.
- The `prelude` SDK / init world as a bundle of capabilities (only a re-export module exists).
- Converting the std packages (`core`, `collections`, `tasks`, ...) into SDK packages.
- Updating `docs/component-worlds/` to the `core` exception (see 01).

**Decision needed.** Package split names (`core`, `collections`, `tasks`, others) and which
capabilities the init world bundles by default.
