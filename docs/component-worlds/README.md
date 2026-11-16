# Module values, environments and component worlds — design draft

Status: proposal, 2026-11-16. Nothing here is implemented. It replaces the module
and loading model described in [module syntax](../services.md),
[module compilation](../modules.md) and [runtime loading](../dynamic-import.md);
where those documents disagree with this draft, this draft is the target. Syntax is
illustrative. `std::fs` and `std::net` are part of the target standard library.
Statuses and errors use [data enums](../data-enums-draft.md), also a draft.

The design answers six questions, one per document:

1. [Paths and module values](01-paths-and-values.md): how static `::` paths and
   first-class module values coexist, and what a package is.
2. [Provided environments](02-provided-environments.md): how an instantiating
   component controls exactly which modules and members another component can reach.
3. [Worlds and SDKs](03-worlds-and-sdk.md): how that control stays visible to
   language servers, type checking and static analysis.
4. [Versioning, loading and lifecycle](04-versioning-and-loading.md): compatibility
   rules, load-time checks, errors, caching, unloading and revocation.
5. [SDK coordination](05-sdk-coordination.md): layered SDKs, interface digests and
   the load handshake that tells both sides whether they agree.
6. [Components](06-components.md): one concept for every loadable unit,
   recipes, generative instances, catalogs, and the root component.

## Terms

| Term | Meaning |
| --- | --- |
| Package | Source layout on disk: a directory with `cyallo.toml` |
| Recipe | A value describing a component (world, files, imports, bundled dependencies), built in code or read from a package |
| SDK | Worlds and interface declarations only |
| Component | A checked recipe: code, imports, exports; immutable |
| Instance | A component running under one environment |
| Environment | What an instance's names resolve to |
| Catalog | An immutable set of component versions |

"Plugin", "library" and "host" are roles. A plugin is a component another component
instantiates and calls through a world; a library is a component others import; a
host is whichever component builds an environment for another. The program entry is
the root component, instantiated by the init loader.

## Summary

- `::` resolves at link time and is a compile error when missing. `.` on a module
  value is checked statically when its module type is known, and is a runtime
  lookup on `dynamic`; `std::reflect::get` is the fallible form of that lookup. A
  static module in value position and a loaded instance share one representation.
- A loaded component has no ambient authority. `load` always takes an environment;
  pure std modules are present by default, and authority-bearing modules exist only
  when the environment provides them.
- The contract between two components is a **world**, published in an **SDK** of
  declarations only. The instantiated component is checked against the world; the
  instantiating component checks its environment against the same world, as a
  record literal. Only implementation choice happens at runtime.
- Everything loads from a **recipe**: a value the host builds in code from source it
  obtained however it chose (downloaded, embedded, generated, read from disk). The
  loader never fetches anything itself.
- Instances are generative. Sharing an instance means passing the same value;
  instantiating again isolates. Types declared in a component belong to its
  instance, so only static instances have static types; runtime instances are
  opaque handles.
- A world generates ordinary declarations (`W::Env`, `W::Exports`, `W::load`), so
  typed loading needs no type-level arguments.
- SDKs carry their own version history (`#[since]`, dependency floors) and interface
  digests, so a mismatch is a precise load error naming the SDK, version or item.
  `std` is versioned the same way.

## Rejected directions

| Model | Why not |
| --- | --- |
| Rust as-is | `mod` declarations are ceremony inside a package; modules are not values. |
| JS (ESM) | No global hierarchical namespace; `std::fs::read_to_string` without an import is impossible. |
| Python | Ambient search path, side-effecting imports, mutable module objects. |
| Java class loaders | Lazy linking (`ClassNotFoundException` at first use), loader-scoped type identity, classpath shadowing. |
| Fixed roots for loaded components | No fine-grained authority: a component gets all of `std::fs` or none. |
| Untyped environments only | Names depend on runtime values; editors and checkers cannot see them. |
| Libraries requested through an SDK procedure | Runtime-resolved names, late failure, types unusable in signatures, authority invisible. |
| Implicitly shared instances | State inside a shared instance makes one consumer's effects observable to another. |
| Statically typed runtime instances | Needs types that depend on runtime values; otherwise a fresh instance's types are confused with the caller's own. |
| Automatic fetching by the loader | Where code comes from is a host decision, not a resolver's. |

## References

- Zig: files are structs, `@import` returns a namespace value.
- Erlang/Elixir: fully qualified remote calls without imports; two-version hot reload.
- WebAssembly component model: components, worlds with typed imports and exports.
- SES Compartments, Deno permissions: host-controlled module maps and authority.
- ML functors and abstract types: generative instantiation, opaque types.
