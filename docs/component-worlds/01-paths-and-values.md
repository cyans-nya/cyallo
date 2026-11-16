# Paths and module values

Part of the [component worlds draft](README.md).

## Problem

Rust's paths fit cyallo, but three Rust assumptions do not suit a dynamic language:

1. `mod foo;` must be declared in a parent before a file participates.
2. Modules are not values: `let fs = std::fs` is not an expression.
3. Visibility detail (`pub(crate)`, split type/value namespaces) adds friction
   without much benefit here.

A statically imported module and a runtime-loaded module should be one kind of
thing.

## Design

One kind of module, two ways to reach it, distinguished by the operator:

```cyallo
std::fs::read_to_string("a.txt")   // full path, no import
use std::fs                        // `use` only creates a local alias
fs::read_to_string("a.txt")

let m = std::fs                    // static module in value position
m.read_to_string("a.txt")          // checked statically: `m` is known to be `std::fs`

let recipe = std::module::Recipe::read("extension.ya")?
let env = std::module::Env::inherit()
let p = std::module::load(recipe, env, std::module::Supply::none())?.namespace()
p.twice(21)                        // `p: dynamic`: runtime lookup
```

- **`::` resolves at link time.** A missing path is a compile (or load) error.
- **`.` on a static module value** is checked like a field access. A module path in
  value position has the module's own static type: exactly the public members of
  that module. A missing member is a compile error and a call compiles like
  `P::member(...)`. The type is inferred; this draft gives it no written form.
- **`.` on `dynamic`** is a runtime lookup. A missing member is a typed runtime
  failure, like any missing read on a `dynamic` object.
- **`std::reflect::get` on `dynamic`** is the same lookup returning a `Result`, for
  code that expects members to be absent ([below](#fallible-lookup-on-dynamic)).

Only static paths have module types. An instance created at runtime is a different
instance with its own types, and cyallo has no types that depend on a runtime value,
so runtime instances are opaque `std::module::Instance` handles
([06](06-components.md#static-types-come-only-from-what-a-component-statically-has)).
`instance.namespace()` gives their `dynamic` namespace object, and instances loaded
against a world have that world's generated `W::Exports` type
([03](03-worlds-and-sdk.md#instantiating-side)).

At runtime every module value is the same readonly, frozen namespace object,
whether it came from a static path or from a loaded instance.

### Fallible lookup on `dynamic`

`.` and `[key]` on `dynamic` treat a missing member as a failure: the right default
when the code's author knows the member is there. Code inspecting a namespace it did
not write (an untyped instance, an optional export, a probe for a feature) needs
absence as a value. `std::reflect` provides that:

```cyallo
// std/reflect.ya
pub open enum ReflectError {
    Missing: record { key: string },   // no member with that key
    NotObject,                         // the value has no members (an erased integer, say)
}

pub proc get(read value: dynamic, read key: string) -> Result<dynamic, ReflectError>;
```

```cyallo
match std::reflect::get(p, "twice") {
    Ok(twice) => std::io::print(twice(21)),
    Err(ReflectError::Missing { key }) => std::io::print("no `", key, "` export"),
    Err(_) => std::io::print("not a namespace"),
}

type Tools = object { version: i64, .. }
let tools = std::object::view<Tools>(std::reflect::get(p, "tools")?)?   // fallible get, then a typed view
```

- **Same lookup as `.`.** `get(value, key)` returns exactly what `value[key]` would,
  including callable values, child namespaces and type descriptors, and fails
  exactly where it would fail. Only the failure's form differs: a `ReflectError`
  value instead of a runtime failure.
- **No extra reach.** Private members of a namespace are absent, reported as
  `Missing` and indistinguishable from members that do not exist, so reflection
  reveals nothing `.` would not.
- **No extra effects.** A get performs the same read `.` would, and nothing more.
- **Pure.** `std::reflect` is a pure module, present in every environment
  ([02](02-provided-environments.md)); it grants nothing a value's holder lacks.
- **A [data enum](../data-enums-draft.md) error.** `ReflectError` is `open`, so new
  cases can be added in a minor version; [types as values](../types-as-values-draft.md#using-types-stdreflect)
  adds the rest of `std::reflect`.
- **Typed after the get.** `std::object::view<T>` turns a found member into a
  checked structural type, as for any `dynamic` value.

### Members of a module value

- Functions and procedures are callable values bound to their declaration
  identity, including declarations implemented by the runtime.
- A generic member instantiates at each call from the actual argument types. A
  generic whose type parameters cannot be inferred from its arguments is reachable
  through `::` with explicit type arguments, not through `.` on `dynamic`.
- Types are runtime type descriptors. On a static module value they are also static
  type names (`m.Error`); through `dynamic` they are not.
- Child modules are module values.

Naming a module in value position needs the same authority as naming one of its
members with `::`. Inside a loaded component, `std::fs` is whatever its
environment provides ([02](02-provided-environments.md)), so `let m = std::fs`
yields the provided module.

## Packages

A **package** is a directory containing `cyallo.toml`. It is the on-disk form of a
[recipe](06-components.md#recipes); a recipe built in code has the same structure.
Its module tree is its file
tree: `foo.ya` or `foo/mod.ya` is module `foo` (both present is an error), and
`foo/bar.ya` is `foo::bar`. A subdirectory with its own `cyallo.toml` is a separate
package and not part of the tree. The manifest names the root file (default
`lib.ya`, or `main.ya` for an executable).

A `.ya` file outside any package is a **single-file package**. Its tree is the file
plus whatever it declares with `mod foo;` or inline `mod foo { ... }`. This keeps
scripts and directories of independent examples working without each sibling file
becoming a module of the others.

The whole package links at once: every file in the tree is parsed, resolved and
checked, and a package with an error in any file fails to build or load. The
package's namespace object is complete, so `.` on a child module never finds a
module that was skipped.

## Proposed changes

1. Inside a package, the file tree is the module tree and `mod foo;` is not needed.
   Single-file packages keep `mod foo;`. Inline `mod name { ... }` stays everywhere.
2. Allow a module path in value position, producing a value with that module's
   static type.
3. Add `std::reflect::get`, the fallible form of a `dynamic` lookup.
4. Collapse visibility to `pub` (exported from the package) and private (visible
   anywhere in the package). `pub(crate)` disappears. `crate::`, `self::` and
   `super::` remain path anchors; they carry no visibility rules. Code that relied
   on privacy to a module and its descendants uses an inline `mod` in its own
   package, or moves to its own package.

## Runtime loading without class-loader failures

| Java failure | Cause | Rule |
| --- | --- | --- |
| `ClassNotFoundException` mid-execution | Lazy linking | Link the whole package at load; `load` returns a `Result`. |
| `ClassCastException` between "same" classes | Identity = (loader, name) | Every load creates a new instance; types belong to their declaring instance and sharing is explicit ([06](06-components.md)). |
| Classpath shadowing | Ambient search path | No search path; explicit roots and environments. |
| Surprise static initializers | Loading runs code | Module initialization permits constants only. |
| Loader leaks | Loaders pinned by classes | A namespace is an ordinary value; code is reclaimed when nothing refers to it. |

### Unloading

Dropping the last reference to a loaded namespace unloads it. The runtime must
therefore publish loaded code in reclaimable segments. A segment stays alive while
any namespace object, function value, closure, trait object, suspended frame or
generator refers to code or globals in it. The cache ([04](04-versioning-and-loading.md))
holds checked code only, never instances, so caching never keeps an instance alive.

### Hot reload

If added, reload follows Erlang: `reload` returns a new namespace, and holders of
the old namespace keep the old code. Unlike Erlang, the number of live versions is
not capped: an old version lives exactly as long as something refers to it, and is
reclaimed by the rule above.
