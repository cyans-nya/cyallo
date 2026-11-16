# Worlds and SDKs

Part of the [component worlds draft](README.md).

## Principle

Move the contract into source. A **world** declares a component's imports and
exports. The instantiated component and the instantiating component are both
checked statically against it; only the implementation is chosen at load.

## SDK packages

An SDK contains worlds and interface modules only: no bodies, globals or
initializers. It is the only artifact both sides must share.

```cyallo
// host_sdk/lib.ya  (package host_sdk, version 1.2.0)
pub interface mod api {
    pub opaque record Context;
    impl Context {
        pub fn data_dir(self: &Self) -> string;
        pub proc log(self: &Self, message: string);
    }

    pub record Error { message: string }

    pub trait Instance {
        proc deactivate(self: Self);
    }
}

pub world Base {
    import std::io;
}

pub world Extension {
    include Base;
    import std::fs {
        fn read_to_string(read path: string) -> Result<string, std::fs::Error>;
    }
    import api;
    export proc activate(ctx: api::Context) -> Result<dyn api::Instance, api::Error>;
}
```

Interface declarations use the same body-less form as other std declarations.
`include` composes worlds. Inside an interface module:

- `opaque record` is abstract: the implementing component chooses its
  representation.
- `record` is a transparent data type defined by the SDK. Its fields are part of
  the interface and of its digest ([05](05-sdk-coordination.md)).
- `trait` declares behaviour the instantiated component implements and the
  instantiating component calls.

## Instantiated side

```toml
# cyallo.toml
[world]
sdk = "host_sdk@^1.2"
name = "Extension"
```

```cyallo
// lib.ya
use host_sdk::api

record Watcher { ctx: api::Context }

impl api::Instance for Watcher {
    proc deactivate(self: Self) { self.ctx.log("stopped") }
}

pub proc activate(ctx: api::Context) -> Result<dyn api::Instance, api::Error> {
    let config = std::fs::read_to_string("data/config")   // signature known to the LSP
    std::fs::write("x", "")                               // static error: not imported by world
    ctx.log("started")
    return Ok(Watcher { ctx: ctx } as dyn api::Instance)
}
```

The manifest lets the language server find the SDK before any instantiating
component exists. The world is the component's entire namespace beyond pure std and
its own component imports ([06](06-components.md#worlds-of-components)). World
imports appear at their own paths: `std::` imports under `std`, and the SDK's
interface modules under the SDK's package name (`host_sdk::api`).

## Instantiating side

A world `W` generates ordinary declarations, so using it needs no world-valued type
arguments:

| Declaration | Meaning |
| --- | --- |
| `W::Env` | A record type with one field per import, including imports from `include`d worlds |
| `W::Exports` | The type of an instance loaded against `W`: one member per export |
| `W::load(recipe, env, supply)` | `Result<W::Exports, std::module::LoadError>` |

A field of `W::Env` is named by the import's last path segment, or by its alias
(`import other::io as other_io;`); two imports with the same name and no alias are
an error in the world. Field types are generated from the imports and never
written by hand.

```cyallo
impl interface host_sdk::api for crate::api_impl {
    type Context = crate::ExtensionContext;
}

let env = host_sdk::Extension::Env {
    io: std::io,
    fs: crate::sandbox_fs,
    api: crate::api_impl,
}
let recipe = std::module::Recipe::read("extensions/watcher")?
let ext = host_sdk::Extension::load(recipe, env, std::module::Supply::none())?
let instance = ext.activate(crate::ExtensionContext::new("watcher"))?   // typed, not `dynamic`
// ...
instance.deactivate()
```

`W::Env { ... }` is an ordinary record literal, so the compiler checks it like one:
every import has a field, no field is unknown, and every value conforms to its
field's type.

- A std or interface import takes a static module path in value position, checked
  at compile time. A runtime `std::module::Instance` is accepted too and checked at
  load, since its types are not known statically.
- A component import takes the instantiating component's own static instance
  ([below](#worlds-importing-components)).

A record value cannot gain fields at runtime, so the presence check is complete.

## Worlds importing components

A world can import a component, not only std modules and interface modules:

```cyallo
pub world Indexer {
    include Extension;
    import json@^2;
    export proc index(document: json::Value) -> Result<(), api::Error>;
}
```

The import becomes a field of `W::Env` (`json`), and the component's types may
appear in world signatures. Both sides must name the same `json::Value`, which means
the same instance ([06](06-components.md#type-identity-follows-instances)). Only a
static instance has types known at compile time, so the field accepts only the
instantiating component's own instance of that component, written as its path:

```cyallo
let env = host_sdk::Indexer::Env { io: std::io, fs: crate::sandbox_fs, api: crate::api_impl, json: json }
```

The instantiating component must therefore import `json` itself, in a version that
satisfies the world's range. Component imports that appear only in a recipe are
supplied at load instead ([06](06-components.md#supplying-component-imports)).

## Who sees what

| Party | Checked against | When |
| --- | --- | --- |
| Instantiated component's editor / LSP | World imports and exports | Editing, build |
| Instantiating component's compiler | World imports (`W::Env`) and exports (`W::Exports`) | Its build |
| Loader | Handshake ([05](05-sdk-coordination.md)), then component against world | Load |

## Naming rules

- **`std::` imports are exact subsets of real std.** When the SDK is compiled, every
  listed signature is checked against the real module. A provision may refuse more
  inputs, never change meaning. The checker cannot verify this; it is a contract on
  whoever writes provisions, and anything with different meaning belongs under the
  SDK's own names.
- **Different semantics live under the SDK's own names** (`api::...`).

## Conformance

A provision satisfies an import if it has every declared member with a matching
signature. Extra members are allowed and invisible to the loaded component. Nominal
types in signatures must share identity, so wrappers reuse std types; specific types
belong in interface modules. A component satisfies a trait-object export when its
type implements the SDK trait.

## Opaque types

Interface opaque records are abstract to the components that import them, which can
pass and use them only through interface functions. Identity is
`(SDK package, major version, path, implementing instance)`
([06](06-components.md#type-identity-follows-instances)).

- `impl interface host_sdk::api for M { type Context = T; }` binds `Context` to `T`
  in the instance that contains `M`. Inside that instance,
  `host_sdk::api::Context` and `T` are the same type.
- An instance binds each opaque type at most once per SDK major. Other modules in
  the same instance implementing the same interface (wrappers, attenuated variants)
  write `impl interface host_sdk::api for crate::readonly_api` without type
  bindings and use the existing one.
- A wrapper in another instance that forwards to an implementation binds its opaque
  types to that implementation's (`type Context = inner::Context`), and so shares
  its identity.

So a value of an opaque type has one representation wherever its identity appears.
Two components loaded with the same implementing instance agree on `api::Context`;
components loaded against different implementations do not, and mixing them fails at
link time. Per-component data belongs in fields of the bound type (`ExtensionContext`).

## Closed exports

For a component with an SDK world, the instantiating component sees only world
exports, and they are its only entry points. Its other `pub` items are internal.
Worlds export procedures, functions and constants; a component's own types reach
the other side only behind SDK traits or as SDK types. `W::load` returns a
`W::Exports`; no `dynamic` crosses the boundary.

## No declared world

A component without an SDK world has an implicit world whose exports are its `pub`
items ([06](06-components.md#worlds-of-components)). It is loaded with an untyped
environment ([02](02-provided-environments.md)) through `std::module::load`, or
instantiated from a catalog. Both return an opaque `std::module::Instance`, and its
imports are checked at load against the modules actually provided. Code that wants
to call it statically imports it as a component instead.
