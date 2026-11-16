# SDK coordination between components

Part of the [component worlds draft](README.md).

## Goals

1. A common SDK layer can be shared by several other SDKs, so components written
   against it load into any component that implements it, and its types mean the
   same thing everywhere.
2. The instantiating and instantiated components agree, before any loaded code
   runs, on which SDK, version and interface each was built for. Disagreement is a
   precise load error, not a check failure buried in source or a runtime fault.

## Layered SDKs

SDKs are ordinary packages and may depend on each other. A common core is published
once and reused:

```cyallo
// ext_core/lib.ya  (package ext_core 1.1.0)
pub interface mod log {
    pub opaque record Logger;
    impl Logger {
        #[since("1.0")] pub proc info(self: &Self, message: string);
        #[since("1.1")] pub proc warn(self: &Self, message: string);
    }
}
pub interface mod event {
    pub record Event { name: string, payload: string }
}
pub world Core {
    import std::io;
    import log;
    import event;
}
```

```cyallo
// host_sdk/lib.ya  (package host_sdk 1.4.0, depends on ext_core ^1.1)
pub use ext_core::{log, event};       // reexport, identities unchanged

pub interface mod api {
    pub opaque record Context;
    impl Context {
        pub fn logger(self: &Self) -> ext_core::log::Logger;
    }
    pub record Error { message: string }
    pub trait Instance {
        proc deactivate(self: Self);
    }
}

pub world Extension {
    include ext_core::Core;
    import api;
    export proc activate(ctx: api::Context) -> Result<dyn api::Instance, api::Error>;
}
```

Rules:

- **Identity belongs to the defining SDK.** `ext_core::event::Event` has identity
  `(ext_core, major 1, event::Event)` whether reached directly or through
  `host_sdk::event`. Reexports never mint new identities. Opaque types add the
  implementing instance ([03](03-worlds-and-sdk.md#opaque-types)).
- **One version per SDK major per implementing component.** Like Cargo's
  resolution: within a component that implements SDKs, every SDK it implements that
  depends on `ext_core@1` unifies to the single `ext_core@1` version it
  implements. Transparent core values pass between any components on that major;
  opaque core values pass between components loaded with the same implementing
  instance, even when they target different worlds.
- **A component may target the core world directly** (`sdk = "ext_core@^1"`,
  `name = "Core"`) and then loads into any component that implements
  `ext_core@1`.
- **An instance binds each SDK major's opaque types once.**
  `impl interface ext_core::log for ...` with its type bindings is shared by
  every world that includes `ext_core::Core`.

## `std` is an SDK

Worlds import `std` items, so `std` is versioned like any SDK. Its declarations carry
`#[since]` and its items have digests. The init loader implements exactly one `std`
version per major for the root component ([06](06-components.md#the-root-component-and-the-init-loader)),
and environments pass it on. A component declares its minimum `std` in its recipe,
and a component built with a newer toolchain loads under an older one as long as it
uses nothing newer than that minimum.

## Version history in SDK artifacts

An SDK artifact describes its own history, so the checker needs no registry to
reason about older versions:

- Every item carries `#[since]`: the minor version that introduced it.
- Every dependency carries its range history: the range each earlier minor of this
  SDK required.

```toml
# host_sdk/cyallo.toml
[package]
name = "host_sdk"
version = "1.4.0"

[dependencies.ext_core]
version = "^1.1"
history = { "1.0" = "^1.0", "1.3" = "^1.1" }   # host_sdk 1.0–1.2 required ^1.0
```

When the previous published artifact of the same major is available, the SDK
compiler checks the new one against it: every carried-over item has an unchanged
digest, every new item has `#[since]` equal to the new version, nothing is removed,
and the dependency history only extends. A minor that breaks these rules is
rejected before publication. Components' exported interfaces follow the same rules
([04](04-versioning-and-loading.md#semver-for-components)).

## Interface digests

Compiling an SDK or a component produces digests of its interface, not its source
text: comments, formatting and declaration order do not affect them.

- **Item digest:** canonical form of one declaration: path, kind, signature, effect
  contract, receiver contract, referenced type identities, `#[since]` version,
  transparent record fields and trait method signatures. Opaque types digest as
  their identity only.
- **World digest:** the sorted item digests of everything the world imports and
  exports, including `include`d worlds and dependency SDKs.

Each published `(sdk, version)` has exactly one digest set. Two artifacts with the
same version and different digests are inconsistent.

## What each side records

**Instantiated component.** The recipe declares ranges; the build writes a
lockfile that pins what was checked and records the **use set**: the digest of every
SDK and component item the source references. On disk these are `cyallo.toml` and
`cyallo.lock`; a recipe built in code carries the same data (`.world`, `.import`,
`.lock`, see [06](06-components.md#recipes)).

```toml
# cyallo.toml
[package]
std = "^0.9"

[world]
sdk = "host_sdk@^1.2"
name = "Extension"

[imports]
json = "^2.1"
```

```toml
# cyallo.lock (generated by the build)
[[sdk]]
name = "host_sdk"
version = "1.4.0"
world = "Extension"
world_digest = "b3:7f1c..."
uses = { "api::Context::logger" = "b3:41e0...", "api::Instance" = "b3:9c2a..." }

[[sdk]]
name = "ext_core"
version = "1.1.0"
uses = { "log::Logger::info" = "b3:5d07..." }

[[sdk]]
name = "std"
version = "0.9.3"
uses = { "fs::read_to_string" = "b3:e218..." }

[[component]]
name = "json"
version = "2.3.0"
uses = { "parse" = "b3:a4c1...", "Value" = "b3:17be..." }
```

The lockfile comes with the recipe and is untrusted. The loader uses it
only to give early, specific diagnostics; checking the source remains the authority.

**Implementing component.** Its recipe lists the exact SDK versions it implements;
the compiler embeds an implementation table, with every item digest, in its
compiled form:

```toml
[implements]
host_sdk = "1.4.0"
ext_core = "1.1.0"
```

`impl interface` blocks are checked against these exact versions when it builds.
For the root component, `std` is implemented by the init loader.

## Minimum version and `#[since]`

The lower bound of each range in a component's recipe is its **minimum
implementation version** for that SDK. Dependency SDKs get their minimum through the
history: `host_sdk ^1.2` permits `host_sdk 1.2`, which required `ext_core ^1.0`,
so the component's minimum `ext_core` is 1.0. The checker and language server
reject use of an item whose `#[since]` is newer than the minimum:

```text
error: `ext_core::log::Logger::warn` requires ext_core 1.1, but this component
       declares host_sdk ^1.2, and host_sdk 1.2 permits ext_core 1.0
help: raise the range to host_sdk ^1.3, or use `Logger::info`
```

So a component cannot silently depend on a newer implementation than it claims to
support, and the version it was built against (1.4.0 above) does not limit where
it loads. Component imports use the same rule against their `#[since]`.

## Load handshake

Runs before the loaded component is checked:

1. **Name and world.** The instantiating component implements the loaded
   component's SDK and world name.
2. **Version range.** For each SDK the loaded component depends on, directly,
   through SDK dependencies, or `std`, the implemented version satisfies its range.
   For each component import, the supplied instance's version satisfies its range
   ([06](06-components.md#supplying-component-imports)).
3. **Digest agreement.** For each entry in the lockfile's use sets, the implemented
   or supplied item digest equals the recorded one. Items the component does not use
   are ignored, so harmless differences elsewhere do not block loading. If the same
   SDK version has a different digest set, the error is `SdkInconsistent` rather
   than `ItemMismatch`.
4. **Exports.** Required world exports are present (from the world digest).

Then the component is checked against its world as usual
([04](04-versioning-and-loading.md#load-time-checks)).

A recipe without lock data skips step 3. Steps 1, 2 and 4 use the recipe's declared
world and ranges.

## Multiple majors

A component may implement several majors side by side (`host_sdk@1` and
`host_sdk@2`) with separate `impl interface` blocks and type bindings. The loader
selects the one matching the loaded component's range. Types from different majors
are distinct; converting between them is ordinary code, typically an adapter world
implemented on top of the newer one.

## Errors

The handshake reports through the `LoadError` cases listed in
[04](04-versioning-and-loading.md#errors): `WorldMismatch`, `SdkMismatch`,
`ItemMismatch`, `SdkInconsistent` and `MissingExport`. An `ItemMismatch` renders
each differing item and both signatures:

```text
load failed: component uses host_sdk::api::Context::logger as built against host_sdk 1.4.0
  component:   fn logger(self: &Self) -> ext_core::log::Logger
  implemented: fn logger(self: &Self, name: string) -> ext_core::log::Logger
```

## Runtime introspection

`std::module::sdks(instance)` returns the negotiated `(name, version)` pairs of a
loaded instance, and `std::module::implemented_sdks()` those of the calling
component, for logging and support reports. Both need `std::module` and grant no
authority.

## Open questions

- Whether a component may declare several acceptable targets (`host_sdk@^1.2 ||
  ext_core@^1`) and let the loader choose.
- Digest algorithm and canonical form specification.
- Whether a published SDK registry is worth having for discovery and for detecting
  inconsistent artifacts; version checks no longer need one.
- Whether `#[since]` should also gate optional imports (`import?`) automatically.
