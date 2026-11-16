# Versioning, loading and lifecycle

Part of the [component worlds draft](README.md).

## Semver for SDKs

Compatibility follows who must supply what:

| Change | Breaks | Version |
| --- | --- | --- |
| Add a world import, or a member to an imported interface module | Implementers (must provide more) | Minor; the new item carries `#[since]` |
| Add an optional export (`export?`) | Nobody | Minor |
| Add a required export | Components targeting the world (must implement more) | Major |
| Add a method to an SDK trait | Components implementing it | Major |
| Remove an import, member or export | Components using it, callers of it | Major |
| Change a signature | Both | Major |
| Change the fields of a transparent `record` | Both | Major |
| Change the binding of an opaque type | Nobody | None: implementation-only |
| Add a variant to an `open` enum | Nobody: foreign matches have `_` | Minor; the variant carries `#[since]` |
| Add a variant to a closed enum | Exhaustive matches | Major |

Implementing components pin the exact SDK versions they implement (`[implements]` in
[05](05-sdk-coordination.md)), so a new minor never breaks their build until they
choose to adopt it.

A component implementing `host_sdk 1.4` accepts every component whose declared
range contains 1.4. A component's range sets its minimum implementation version,
and its build rejects any use of an item newer than that minimum (`#[since]`, see
05). Optional exports appear to the instantiating side as `Option<...>`.

## Semver for components

A component's exported interface follows the same table: adding a `pub` item is
minor and carries `#[since]`; removing or changing one is major. Private changes,
including a record's private fields, are patch or minor: types belong to their
instance ([06](06-components.md#type-identity-follows-instances)), so values of two
versions never meet and no layout needs to stay stable across versions.

## Nested loading

A component may load others only if its world imports `std::module`. It can build
environments only from modules it can reach, so authority only shrinks.

## Load-time checks

1. **Take the recipe** ([06](06-components.md#recipes)). The loader never fetches
   source; it uses exactly the recipe it is given, however the caller obtained it.
   `Recipe::read` reads a package's files, `cyallo.toml` and `cyallo.lock` within the
   caller's read grant.
2. **Handshake** ([05](05-sdk-coordination.md)): SDK name, world, version and digest
   agreement, before any component code is checked.
3. **Supply component imports** ([06](06-components.md#supplying-component-imports)):
   offered instances, the supply's catalog, then bundled source.
4. **Check** the component against its world. It is untrusted; this always runs at
   load and is never taken from its own build.
5. **Exports.** The checked component defines every non-optional export with its
   declared signature. The handshake's export step only compares recorded digests;
   this step confirms them against source.

Environment conformance is not rechecked for typed loads: the instantiating
component's compiler established it from `W::Env { ... }`. An untyped load
([02](02-provided-environments.md)) has no SDK world, so step 4 checks the component
against the provided modules and steps 2 and 5 do not apply.

## Errors

Load failures are a [data enum](../data-enums-draft.md). It is `open`, so callers
outside std match it with a `_` arm and new cases arrive in minor versions:

```cyallo
pub open enum LoadError {
    NotFound: record { path: string },                           // Recipe::read: package or file missing
    Denied: record { path: string },                             // Recipe::read: outside the read grant
    Check: record { diagnostics: Vec<Diagnostic> },              // fails to check, including a reference outside its world
    WorldMismatch: record { expected: WorldName, found: WorldName },
    SdkMismatch: record { sdk: string, component_range: string, implemented: string },
    ItemMismatch: record { items: Vec<ItemDifference> },         // item digests disagree
    SdkInconsistent: record { sdk: string, version: string },    // same version, different digests
    MissingExport: record { name: string },
    MissingComponent: record { name: string, range: string },    // nothing offered, cataloged or bundled satisfies it
    DeniedComponent: record { name: string },                    // the supply's catalog or bundled policy excluded it
}

pub enum CatalogError {
    Missing: record { name: string, range: string },
    Denied: record { name: string },
}
```

```cyallo
match host_sdk::Extension::load(recipe, env, supply) {
    Ok(ext) => run(ext),
    Err(LoadError::MissingComponent { name, range }) => std::io::print("needs ", name, " ", range),
    Err(LoadError::Check { diagnostics }) => report(diagnostics),
    Err(other) => std::io::print(other.message()),
}
```

`message()` renders any case for people; programs match on the variant. A reference
to something the world does not import is a `Check` diagnostic naming the path, not
a separate case.

## Caching and unloading

Only checked code is cached. Checked products are keyed by (recipe digest,
world digest, environment identity), with environment identity as defined in
[02](02-provided-environments.md). The compiler's checked-module cache can reuse
them across lineages and across instances.

Instances are never cached. Every `load` and every `instantiate` creates a new
instance, even for the same recipe and an equal environment; only the checking
work is reused. An instance is reclaimed when nothing refers to it
([01](01-paths-and-values.md#unloading)). Clearing the cache never changes
behaviour; it only costs re-checking.

## Lifecycle

World exports are the only entry points of a component with an SDK world; there is
no implicit `main`, and module initialization permits constants only. Its state
lives in a value it returns, typically an SDK trait object:

```cyallo
let instance = ext.activate(ctx)?   // component-owned state behind dyn api::Instance
instance.deactivate()                  // consumes it
```

For revocation, back `api::Context` with a service `Context`. Disposing it makes
every revocable `api::` call return its error ([02](02-provided-environments.md#revocation)).

## Fault and resource isolation

Out of scope for this draft. Calls into a loaded component run in the caller's task
and heap, so a component that loops or fails affects its caller like any other
callee. A caller that needs bounds calls the component from a task it spawns with a
budget, and cancels that task.

## Open questions

- Should `export?` and optional imports (`import? std::net`) both exist, and how
  does a component guard an optional import statically?
- Default methods on SDK traits, so adding a method can be a minor change.
- Should `W::load` offer to place a component in its own task with a budget?
- Signed, pre-checked component artifacts that skip load-time checking.
- Native compilation of loaded code.
