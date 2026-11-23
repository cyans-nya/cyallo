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
7. [Demo](07-demo.md): one project that should work once this is implemented;
   the file-module part already runs.

## Revision 2026-11-23

Decisions from the module-model discussion. Where they differ from the parts below,
this section is the target for the first cut. Nothing here is implemented.

- **One loading primitive.** `std::module::load(source, bindings)`. `Source` is a
  file-tree value. `Bindings` maps paths to modules or instances: a std module, a
  substituted wrapper, or another component's instance. A std import and a
  component import are the same call. This replaces `Env` plus `Supply`. `Catalog`
  and bundled fallback leave the first cut: a catalog is a name-to-`Source` map the
  host writes.
- **Loader trait and explicit entry.** `std::module::load` takes a loader with an
  explicit entry (`loader.entry("lib.ya")`); no filename is a default. The trait:

  ```cyallo
  pub trait Loader {
      proc read(self: &Self, path: string) -> Option<string>   // required
      proc exists(self: &Self, path: string) -> bool           // optional; defaults to read(path).is_some()
      fn index(self: &Self) -> Option<Index>                   // optional: paths, sizes, digests
  }
  ```

  `Source` (in memory, built with `Source::new().path(p, text)`), directory, pack and
  lazy-fetch loaders implement it. Modules are found by probing (`b.ya`, `b/mod.ya`),
  so nothing needs listing. The runtime hashes each file as it reads it, so a lying
  loader cannot poison the checked-code cache. A supplied `index` adds an early
  whole-component cache hit, per-file verification (a mismatch is not found) and
  early size limits. The runtime maps any failure to one not-found for the child. A
  loader runs with the host's authority. With a lazy loader an unreferenced file is
  never read or checked; `cyallo run` still scans its directory. User-written
  loaders need the compiler to re-enter the VM mid-link, so the first cut ships
  native implementations only.
- **Packed modules.** `Source::unpack(bytes)` and `source.pack()` use a `.yamod`
  container (deterministic sections, explicit tags, widths and offsets). Compression
  such as `.zst` is an outer layer with a bounded output size. Identity is the digest
  of the canonical file tree, not the compressed bytes. Paths are normalized and
  relative, with no `..`, duplicates or case-insensitive collisions. Files compress
  separately, so a lazy loader can read the index first. The first cut ships source
  only, so the runtime always checks it.
- **The loader knows nothing about `cyallo.toml` and touches no filesystem.** A
  std helper, `Manifest::parse(text)`, turns TOML into a `Manifest` value (native,
  bounded, typed `ManifestError`). `Source::read(path)` uses the caller's `std::fs`
  authority. The host reads the manifest, locates or fetches dependencies by its own
  policy, builds the bindings and calls `load`. `Range::satisfies` (semver) lives in
  std. The manifest is an untrusted claim; authority is only the bindings.
- **Least privilege across components.** A child gets exactly its bindings.
  `Bindings::inherit()` is explicit sugar. Files of one component share its
  authority and form no boundary.
- **Attenuation is structural.** A child can only receive, or pass on, bindings its
  parent holds, so no subset check exists or is needed.
- **Uniform not-found.** An unbound name, a path outside the caller's `std::fs`
  scope, an absent file and an escaping symlink are indistinguishable to the loaded
  component. `LoadError::DeniedComponent` collapses into not-found for the child;
  detail goes only to the host. The scope is the host's own (possibly wrapped)
  `std::fs`, so the loader has no separate prefix policy.
- **Entry modes.** A `.ya` entry is a module file that declares `proc main()`, and
  its top level allows only declarations, `const` and immutable `let`. A `.yas`
  entry is a script whose top-level statements are the program and which never
  looks for `main`. Neither falls back to the other. For now a script is limited to
  std and files. A PEP 723-style comment block (`// /// script` ... `// ///`
  holding TOML) is reserved, read by a host helper such as `Manifest::from_script`;
  the runtime needs nothing for it. See
  [entry modes](../data-oriented-ir-design.md#entry-modes-ya-modules-and-yas-scripts).
- **Modules.** Files are modules, `d/mod.ya` gates its directory's children until
  it writes `pub use b;`, and inline `mod name { ... }` stays. See the
  [module model](../data-oriented-ir-design.md#module-model-and-batched-construction).
- **Init host.** `cyallo run foo.ya` builds the root's default world natively in
  Rust (all std authority plus the CLI's grants). The same Rust function backs
  `Manifest::parse`, so the CLI and std cannot drift.

| Native | Cyallo (std source) |
| --- | --- |
| `load`, `Source::read`, `Manifest::parse` / `from_script`, content digests | `Bindings` (immutable map), `Range::satisfies`, catalog and locate helpers, the usual host loop |

```cyallo
let manifest = std::module::Manifest::parse(std::fs::read_to_string("plugins/watcher/cyallo.toml")?)?
let source = std::module::Source::read("plugins/watcher")?

let mut bindings = std::module::Bindings::none().bind("std::io", std::io)
for dep in manifest.imports {
    bindings = bindings.bind(dep.name, crate::locate(dep)?)   // host policy
}
let child = std::module::load(source.entry("lib.ya"), bindings)?
```

Open: whether `Env::empty()` (nothing, not even pure std) survives as
`Bindings::none()` or pure std is always present; whether a script may name its
dependencies through a sibling `cyallo.toml`.

## Terms

| Term | Meaning |
| --- | --- |
| Package | Source layout on disk: a directory with `cyallo.toml` |
| Recipe | The earlier draft's value combining files and manifest claims. In the [revision](#revision-2026-11-23) it is split into `Source` (files) and `Manifest` (host-side data) |
| Source | A file-tree value a component is loaded from |
| Manifest | Parsed `cyallo.toml` data read by the host; the loader never sees it |
| Bindings | The paths a child can resolve beyond its own files, mapped to modules or instances |
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
- Everything loads from a **source** (earlier drafts: a recipe) the host builds from
  text it obtained however it chose (downloaded, embedded, generated, read from
  disk). The loader never fetches anything and never reads a manifest itself.
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
