# New design drafts

Drafts added or reworked on 2026-11-16. None of them is implemented; each states its
status at the top. They depend on each other in the order listed.

## [Component worlds](component-worlds/README.md)

Modules as values, no ambient authority, and worlds as the typed contract between
components. Reworked from the earlier plugin-worlds draft (renamed from
`docs/plugin-worlds/`); 06 is new.

| Document | Covers |
| --- | --- |
| [README](component-worlds/README.md) | Summary, terms, rejected directions |
| [01 Paths and module values](component-worlds/01-paths-and-values.md) | `::` versus `.`, packages, unloading, `std::reflect::get` |
| [02 Provided environments](component-worlds/02-provided-environments.md) | Untyped environments, pure versus authority modules, revocation |
| [03 Worlds and SDKs](component-worlds/03-worlds-and-sdk.md) | Worlds, generated `W::Env` / `W::Exports` / `W::load`, opaque types |
| [04 Versioning, loading and lifecycle](component-worlds/04-versioning-and-loading.md) | Semver, load-time checks, `LoadError`, caching, lifecycle |
| [05 SDK coordination](component-worlds/05-sdk-coordination.md) | Layered SDKs, `#[since]`, digests, lockfiles, load handshake |
| [06 Components](component-worlds/06-components.md) | Components, recipes, generative instances, catalogs, the root component |

## [Data enums and anonymous records](data-enums-draft.md)

`enum` declarations with `Variant: Type` payloads, `match` and `if let`, open enums
and versioning, anonymous structural `record { ... }` types, and migrating std
statuses and errors to enums.

## [Types as values](types-as-values-draft.md)

Zig-style compile-time types (`type` values, type functions, `std::meta`,
`const for`), runtime reflection through `std::reflect`, and building types at
runtime with `std::meta::TypeBuilder`, allowed but discouraged.
