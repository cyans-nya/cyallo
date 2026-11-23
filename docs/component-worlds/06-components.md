# Components, instances, recipes and catalogs

Part of the [component worlds draft](README.md).

## Problem

An extension may use a common library, a library may use another, and an extension
may use another extension. These are one concept: a component in different roles. The instantiating side
decides where components come from, which ones a component receives, and whether
they are shared, restricted or updated, without hidden state deciding who sees
whose effects.

## Terms

| Term | Meaning |
| --- | --- |
| Package | Source layout on disk: a directory with `cyallo.toml` ([01](01-paths-and-values.md#packages)) |
| Recipe | A value describing a component: world, files, imports, bundled dependencies. A package on disk is one way to obtain a recipe. |
| SDK | Worlds and interface declarations only; implemented by whoever instantiates against it |
| Component | A checked recipe: code, imports, exports. Immutable; holds no state. |
| Instance | A component running under one environment. Created by instantiation. |
| Catalog | An immutable set of components |

"Plugin" and "library" are roles, not kinds. A plugin is a component that another
component instantiates and calls through a world. A library is a component other
components import. One component can be both. "Host" is the instantiating role: any
component that builds an environment for another.

## Revised first cut (2026-11-23)

The [revision](README.md#revision-2026-11-23) supersedes the recipe, supply and
catalog sections below for the first cut. A component is loaded with
`std::module::load(loader.entry(path), bindings)`. A `Loader` (the in-memory `Source`,
a directory, a pack or a lazy fetch) supplies the files and `Bindings` is
the only source of names beyond pure std and the component's own files. A
component import is one more binding, so there is no separate `Supply`. The host
reads the manifest with `Manifest::parse`, resolves dependencies by its own policy
and builds the bindings. A catalog is a plain name-to-`Source` map in host code,
and bundled fallback is dropped. Instances stay generative, and type identity
follows instances as described below.

The init host for `cyallo run`: a `.ya` entry's `proc main()` is the root's export
under a default world that grants all of std plus the CLI's grants; a `.yas` script
runs its top-level statements instead. Both are built natively in Rust. This
replaces "the root file's top-level code is its entry point" in the root section
below.

## Recipes (earlier draft)

Everything that loads or instantiates takes a `std::module::Recipe`. How its source
text was obtained is the host's business: downloaded with the host's own
`std::net` code, read from a cache it manages, embedded as a constant, generated, or
read from disk. The loader never fetches anything itself: no network resolution, no
registry download, no search path.

A recipe is built in code with the same structure a package has on disk:

```cyallo
const PARSE_SOURCE = "pub fn parse(read text: string) -> ..."   // bundled with the host

let text = crate::downloads::fetch_verified("watcher", "1.0.0")?   // the host's own code
let json_recipe = std::module::Recipe::read("vendor/json")?

let recipe = std::module::Recipe::new("watcher", "1.0.0")
    .world("host_sdk", "^1.2", "Extension")   // [world]
    .std("^0.9")                              // [package] std
    .file("lib.ya", text)                     // root file
    .file("parse.ya", PARSE_SOURCE)           // module `parse`
    .import("json", "^2.1")                   // [imports]
    .bundle(json_recipe)                      // bundled fallback, itself a recipe
```

| Expression | Corresponds to |
| --- | --- |
| `Recipe::new(name, version)` | `[package]` name and version |
| `.world(sdk, range, name)` | `[world]` |
| `.std(range)` | Minimum `std` |
| `.file(path, text)` | A file in the package tree; `lib.ya` is the root unless `.root(path)` says otherwise |
| `.import(name, range)` | An entry in `[imports]` |
| `.bundle(recipe)` | Bundled source for an import |
| `.lock(text)` | `cyallo.lock` contents, for the handshake ([05](05-sdk-coordination.md#what-each-side-records)) |
| `Recipe::read(path)` | All of the above, read from a package directory or a single `.ya` file |

- **Building a recipe is pure.** It only assembles text and data. `Recipe::read` is
  the one operation that touches files, and it needs the caller's read grant.
- **A recipe is untrusted.** Its name, version, world and lock data are claims. The
  handshake and the check verify them against the source; the host vouches for a
  recipe only by choosing to load it.
- **A recipe's identity is its content.** Its digest covers the manifest data, every
  file's path and text, and bundled recipes' digests. It keys the cache of checked
  code ([04](04-versioning-and-loading.md#caching-and-unloading)).
- A recipe with one file and no world is a single-file package with an implicit
  world.

## Worlds of components

Every component has a world:

- **SDK world.** The recipe names one. Its exports are the world's exports
  ([03](03-worlds-and-sdk.md#closed-exports)).
- **Implicit world.** Otherwise the component's exports are its `pub` items.

Either way, its imports also include the components its recipe imports:

```toml
# watcher/cyallo.toml: the same recipe, written as a manifest
[world]
sdk = "host_sdk@^1.2"
name = "Extension"

[imports]
json = "^2.1"                                 # supplied by the instantiator
http = { version = "^1.0", bundled = true }   # source shipped in this package as a fallback
```

Inside the component, an imported component is reached by its name as a root path
(`json::parse(...)`), checked against its exported interface like any other
import.

## Instances are generative

`instantiate` creates a new instance with its own identity on every call, even
under an equal environment. A component cannot be assumed stateless: state can
enter through stateful provisions, service contexts or callbacks it receives, so
two consumers silently given one instance could observe each other's effects.

- **Sharing is passing a value.** Giving the same instance to several environments
  shares it; instantiating again isolates. The choice is visible in source and
  recorded nowhere else.
- **Instances live by reference.** An instance is reclaimed when nothing refers to
  it ([01](01-paths-and-values.md#unloading)).
- **Only checked code is cached.** Instances share checked routines, never state
  ([04](04-versioning-and-loading.md#caching-and-unloading)).

## Static types come only from what a component statically has

A component's own imports are static: `json` in value position is the component's
own linked instance, and `json::Value` is that instance's type. An instance created
at runtime is a different instance with different types, and cyallo has no types
that depend on a runtime value (Scala's `fresh.Value`). So:

- **Runtime instances are opaque handles.** `component.instantiate(env)` and an
  untyped `load` return a `std::module::Instance`. The instantiating side routes it
  into supplies and environments; the loader checks its component name, version and
  digests during the handshake. `instance.namespace()` gives the `dynamic`
  namespace object for runtime lookups; `std::reflect::get` probes it without
  failing ([01](01-paths-and-values.md#fallible-lookup-on-dynamic)).
- **Static calls into an instance happen in the component that imports it**, where
  it really is that component's `json`.
- **Sharing types with what you load means sharing your own instance.** A world that
  imports a component ([03](03-worlds-and-sdk.md#worlds-importing-components)) is
  satisfied with the instantiating component's own static instance, so both sides
  name the same `json::Value`.

Fresh instances are for isolation, and isolated instances cannot exchange types
anyway, so nothing is lost.

```cyallo
let catalog = std::module::catalog()

let first = host_sdk::Extension::load(watcher_recipe, watcher_env,
    std::module::Supply::none().instance(json))?       // `json`: this component's own instance
let second = host_sdk::Extension::load(indexer_recipe, indexer_env,
    std::module::Supply::none().instance(json))?       // same instance: shared with `first`

let isolated = catalog.get("json", "^2")?.instantiate(std::module::Env::pure())?
let third = host_sdk::Extension::load(audit_recipe, audit_env,
    std::module::Supply::none().instance(isolated))?   // a separate instance for `third` only
```

## Type identity follows instances

| Declared in | Identity |
| --- | --- |
| A component | (declaring instance, path) |
| An SDK, transparent `record` | (SDK, major, path) |
| An SDK, `opaque record` | (SDK, major, path, implementing instance) |
| Anywhere, anonymous `record { ... }` | Structural: field names and field types |

Values from isolated instances have distinct types, and mixing them is rejected
where the types meet: at compile time when visible, otherwise at link time. A value
that encodes something meaningful only inside its instance, such as an index into
that instance's table, can therefore never reach another instance.

SDK transparent records have no instance, because an SDK has no code or state, so
their values pass between any components that agree on the SDK major.

[Anonymous records](../data-enums-draft.md#anonymous-records) have no declaring
instance at all. Their values pass between isolated instances whenever their field
types can, so plain data crosses instances as an anonymous record, while a declared
record stays with its instance and can keep invariants. Binding a name to one
(`const Event = record { ... }`) does not change that.

## Supplying component imports (earlier draft)

A recipe's component imports that are not part of its SDK world are not known to
the instantiator statically, so they are satisfied at load from a **supply**:

```cyallo
let supply = std::module::Supply::none()
    .instance(json)                      // offer this instance: shared
    .catalog(catalog.only(["regex"]))    // instantiate requested `regex` fresh
    .bundled(std::module::Bundled::Deny) // never fall back to bundled source
```

`Bundled` is a [data enum](../data-enums-draft.md): `enum Bundled { Allow, Deny }`.

For each such import the loader tries, in order:

1. **An offered instance** whose component name matches and whose version satisfies
   the range: shared.
2. **The supply's catalog**: a fresh instance for this consumer.
3. **A bundled recipe**, if the recipe has one and the supply allows bundled
   fallback: a fresh instance for this consumer.
4. Otherwise the load fails with `LoadError::MissingComponent`, or
   `LoadError::DeniedComponent` when the catalog or policy excluded it.

Fresh instances get the consumer's environment limited to their own declared
imports, plus the same supply for their own component imports. A component can
never reach more than the consumer that imports it.

## Catalogs (earlier draft)

A catalog is an immutable value. Every operation returns a new catalog; nothing
already instantiated changes.

| Operation | Result |
| --- | --- |
| `catalog.add(recipe)` | Checks the recipe and adds the component |
| `catalog.update(name, recipe)` | Replaces the version within its major |
| `catalog.remove(name)` | Removes it |
| `catalog.only(names)` / `catalog.without(names)` | Restricts which components it offers |
| `catalog.attenuate(name, env)` | Instances it creates of `name` run under `env`, intersected with the consumer's |
| `catalog.get(name, range)` | `Result<std::module::Component, CatalogError>` |

Within one catalog each component major has one version. A component that wants a
"current catalog" keeps it in a variable or a service of its own.

### Updating

Linking is eager, so existing instances keep the code they linked against.
Updating a consumer means loading or instantiating it again against the new
catalog, which yields a new instance; the old one is reclaimed when dropped.
Removing a component never cuts off a live instance. Forcible revocation goes
through a service `Context` ([02](02-provided-environments.md#revocation)), which
turns it into error values instead of faults.

## The root component and the init loader

The program entry is a component like any other, instantiated by the **init
loader**: the CLI, or an embedder through the Rust API. The init loader does what
any instantiating component does, with defaults:

- **Recipe.** Read from the entry package or file on disk. An embedder may pass a
  recipe built any other way.
- **World.** The std world `std::program::Main`. It imports all of std; its
  authority is what the init loader grants (the CLI's read grant and future flags
  for network and similar). The root file's top-level code is its entry point.
- **Catalog.** Built from the root's manifest and lockfile, using local sources
  only: path dependencies, the local package cache and vendored sources. Filling
  the package cache is an explicit tool step, never something the runtime does on
  its own. The root reaches the catalog with `std::module::catalog()` when its
  world imports `std::module`.
- **Static dependencies.** For the root's static dependency graph, the init loader
  instantiates each component version once and passes that instance to every
  static dependent, like one copy per version in a Cargo build. This is the init
  loader's stated policy, not hidden state; an embedder may replace it, for example
  to isolate two subtrees.
- **`std` implementation.** The init loader implements the `std` SDK for the root
  ([05](05-sdk-coordination.md#std-is-an-sdk)).

So static imports and runtime loads are one mechanism: a static dependency is a
component import that the init loader satisfied before start; a runtime load is one
that a running component satisfies itself. Both use the same recipe, handshake,
check and instance rules. The Rust embedding API is the only place outside the
language.

## Open questions

- Whether a supply should offer instances by role (any instance satisfying an
  interface) as well as by component name.
- Whether recipes should accept already-checked components as bundled dependencies,
  not only recipes.
