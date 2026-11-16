# Provided environments

Part of the [component worlds draft](README.md).

## Problem

With fixed roots, a loaded component's `std::fs` is the real `std::fs`. An
instantiating component that can only allow or deny whole modules cannot restrict
members, substitute implementations or add capabilities of its own.

## Design

A loaded component has no ambient authority. `load` always takes a
[recipe](06-components.md#recipes) and an **environment**, and the loaded
component's root namespace is whatever that environment provides:

```cyallo
mod sandbox_fs {
    pub fn read_to_string(read path: string) -> Result<string, std::fs::Error> {
        if !std::string::starts_with(path, "data/") {
            return Err(std::fs::Error::denied(path))
        }
        return std::fs::read_to_string(path)
    }
}

let env = std::module::Env::pure()
    .provide("std::fs", crate::sandbox_fs)     // substituted
    .provide("std::io", std::io)               // passed through
    .provide("host::api", crate::extension_api)   // a capability of the instantiating component
                                               // std::net absent
let recipe = std::module::Recipe::read("extension.ya")?
let ext = std::module::load(recipe, env, std::module::Supply::none())?   // std::module::Instance
```

The loaded component uses ordinary names. `std::fs::write` or `std::net::connect` makes the
load fail with a check error naming the missing member.

This is the **untyped** form: an `Env` value built at runtime and checked at load.
When the loaded component declares a world, the instantiating component writes a
**typed** environment instead, a record literal checked at compile time
([03](03-worlds-and-sdk.md#instantiating-side)). Component imports are supplied
separately ([06](06-components.md#supplying-component-imports)).

### Building an untyped environment

| Expression | Result |
| --- | --- |
| `Env::pure()` | Pure std modules only |
| `Env::empty()` | Nothing, not even pure std |
| `Env::inherit()` | Everything the calling component itself can reach |
| `env.provide(path, module)` | A new environment with `path` bound to `module`, replacing any earlier binding |

- The **path** is a string naming a slot in the loaded component's namespace. It is
  parsed when the environment is built and need not exist anywhere in the
  instantiating component (`"host::api"`).
- The **module** is a static module path in value position, or a runtime
  `std::module::Instance`. It is evaluated by the instantiating component, with that
  component's authority. An instance's conformance is checked at load.
- `load` without an environment does not exist. Code that wants "same as me" writes
  `Env::inherit()` explicitly.

## Rules

1. **Pure versus authority modules.** A module is pure when its operations affect
   only values passed to them or created by them. A module that reaches the outside
   world, the runtime or the loader is authority-bearing. A std module that would
   mix both is split.

   | Pure: always present | Authority: present only when provided |
   | --- | --- |
   | `option`, `result`, `string`, `char`, `num`, `array`, `collections`, `cell`, `any`, `object`, `reflect`, `lookup`, `generator`, `gc`, `rc`, `mem`, `channel`, `service` | `fs`, `net`, `io`, `time`, `random`, `task`, `runtime`, `mem::manual`, `module` |

   `channel` and `service` are pure because creating a channel or an empty root
   `Context` grants nothing. Moving values between tasks needs `task`, and reaching
   another component's services needs a `Context` passed in.
2. **The prelude resolves against the environment.** It reexports only modules the
   environment contains. A component without `std::io` has no `io` in its prelude.
3. **`::` resolves against the environment** at the loaded component's link time,
   eagerly. A missing member is a load error, never a mid-call failure.
4. **Provisions are modules**, not arbitrary dynamic objects, so calls stay
   type-checked and keep the static call path. Loosely typed data goes through a
   provided module whose members return `dynamic`.
5. **Attenuation is a wrapper module.** It is ordinary code: path checks, rate
   limits, logging. A member allow-list (`std::fs only { read_to_string }`) is
   shorthand for a generated wrapper.
6. **Authority only shrinks.** A component can build environments only from modules
   it can reach, and `Env::inherit()` inside a loaded component is that component's
   own environment.
7. **Environment identity is structural.** It is the set of (path, provided
   module's declaration identity) pairs, plus the world for typed environments.
   Two environments built separately with the same entries are the same
   environment. It keys the cache of checked code; it never makes two loads share
   an instance ([06](06-components.md#instances-are-generative)).
8. **Types keep their declaring identity.** A provided module keeps its declaration
   identities, so both sides see one `std::fs::Error`; a provided instance's types
   are that instance's types ([06](06-components.md#type-identity-follows-instances)).
   A type that appears in a provided member's signature is reachable at its
   declaring path (`std::fs::Error`) as a type, even when the provision replaces
   that module and does not reexport it. Its constructors and methods are reachable
   only if the environment provides them.

## Remaining problem

The loaded component's names now depend on a runtime value, so its editor and
checker cannot know what `std::fs` contains. [Worlds](03-worlds-and-sdk.md) fix
this.

## Revocation

A provision is fixed for the life of an instance. Revocable access goes through a
wrapper that calls a `ServiceRef`, or through a service `Context` directly: the
environment decides what a component can name; the context decides what is live.

Members of a revocable interface return `Result`. The wrapper maps a withdrawn
service to that error, so revocation is an ordinary value in the loaded component
and never a fault in the instantiating one.
