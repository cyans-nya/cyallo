# Types as values — design draft

Status: proposal, 2026-11-16. Nothing here is implemented. It builds on
[data enums and anonymous records](data-enums-draft.md), on compile-time `const`
evaluation ([function inlining plan](function-inlining-plan.md)), and on
`std::reflect::get` from the [component worlds draft](component-worlds/01-paths-and-values.md#fallible-lookup-on-dynamic).
Syntax is illustrative.

## Goals

1. **Compile-time types, Zig style.** Pure functions compute types, and code
   specializes over a type's structure, with no runtime cost.
2. **Runtime reflection.** Cyallo is a dynamic language, so running code can inspect
   types, resolve them by path, use them to read, write, call and construct values,
   and create new ones.
3. **No slowdown.** Code that does not reflect pays nothing, and reflective code
   reuses the existing descriptors, inline caches and dispatch.
4. **Building types at runtime is allowed but discouraged.** It must stay possible,
   never be the easy path, and never let a program quietly create shapes in a loop.

The split: `std::meta` makes and examines types, and is the normal path in `const`
evaluation. `std::reflect` uses types at runtime and never creates them; only
`std::meta::TypeBuilder` running outside `const` evaluation creates runtime types.

## Identity

| Written as | Identity |
| --- | --- |
| `record Foo { ... }`, `enum Foo { ... }` | Nominal: its declaration. Two `record Foo {}` declarations differ. |
| `record { ... }` | Structural: field names and types |
| `const Foo = record { ... }`, `type Foo = record { ... }` | The structural type it names; `Foo` equals `record { ... }` |
| A named type built in a type function | Nominal: (type function, arguments) |
| A named type built at runtime | Nominal: a new identity per `build()` |
| An anonymous type built at runtime | Structural, interned: equal to every other with the same shape |

## Compile time

### `type` values and type functions

`type` is the type of types. Values of type `type` exist only during compilation:
they appear in `const` evaluation, generic arguments and type positions, never in a
running program. `type X = expr` is short for `const X: type = expr`.

A **type function** is a pure `fn` whose parameters include `type` or other
compile-time values, evaluated by the existing `const` evaluator:

```cyallo
fn Pair(A: type, B: type) -> type {
    return record { first: A, second: B }
}

fn Vector(N: usize, T: type) -> type {
    return [N]T
}

type Point = Pair(f64, f64)          // equals record { first: f64, second: f64 }
type Rgb = Vector(3, u8)
```

- **Evaluated during type preparation**, before contracts are frozen, in
  declaration dependency order. A cycle between type functions and the types they
  produce is a compile error, as for constants.
- **Memoized** on (type function, arguments). `Pair(f64, f64)` is one type
  everywhere. A named type a type function builds has identity (type function,
  arguments).
- **Bounded** by the `const` evaluator's work, recursion and allocation limits.
  Exceeding them is a compile-time diagnostic.
- **Generics stay.** `<T>` is still how routines and declarations abstract over
  types. Inside a generic body, `T` is a `type` value usable in `const` contexts.
- **Value parameters** (`N: usize`) must be compile-time known, as in Zig. The
  existing `Type::Dependent` mechanism used by native declarations is the starting
  point.

### `std::meta`

`std::meta` examines and builds types. Its results are data enums and anonymous
records:

```cyallo
pub enum TypeKind {
    Primitive: Primitive,
    Record: record { name: Option<string>, fields: Vec<Field> },
    Enum: record { name: string, open: bool, variants: Vec<Variant> },
    Tuple: Vec<type>,
    Object: record { fields: Vec<Field>, open: bool },
    Function: record { kind: RoutineKind, parameters: Vec<type>, result: type },
    Reference: record { mutable: bool, pointee: type },
    Managed: record { storage: Storage, locality: Locality, pointee: type },
    Dynamic,
}

type Field = record { name: string, ty: type }
type Variant = record { name: string, payload: type }
```

`std::meta::kind(T)`, `fields(T)`, `variants(T)` and `name(T)` read a type.
`std::meta` sees exactly what the calling code could name: private fields of
another package's records are absent.

`std::meta::TypeBuilder` builds types. In a type function it is the normal way to
make a named type with computed fields:

```cyallo
fn Tagged(T: type) -> type {
    return std::meta::TypeBuilder::record("Tagged")
        .field("tag", string)
        .field("value", T)
        .method("describe", fn(self: &Self) -> string { return self.tag })
        .build()
}
```

### Specializing over structure

`const for` unrolls over a compile-time collection, and `value.[name]` accesses a
field whose name is a compile-time string:

```cyallo
fn write_json<T>(out: &mut Writer, value: &T) {
    const for field in std::meta::fields(T) {
        out.key(field.name)
        write_json(out, &value.[field.name])
    }
}
```

`const if` branches at compile time; the untaken branch is resolved but not
type-checked for that specialization:

```cyallo
const if let TypeKind::Enum(_) = std::meta::kind(T) {
    // ...
}
```

Each specialization gets straight-line code with direct field accesses: no
reflection runs, so there is no runtime cost. This replaces derive macros.
Unrolling is bounded per specialization by an iteration and instruction limit, and
exceeding it is a compile-time diagnostic.

## Runtime

### Using types: `std::reflect`

`std::reflect` is pure and grants no authority. It works on `std::reflect::Type`
descriptors: immutable, Session-owned values, built lazily from Program metadata the
first time code asks for them.

| Operation | Result |
| --- | --- |
| `type_of(value: dynamic)` | `Type` |
| `descriptor<T>()` | The `Type` of a compile-time type |
| `ty.kind()` | `TypeKind`, as in `std::meta`, with `Type` in place of `type` |
| `resolve(namespace: dynamic, path: string)` | `Result<Type, ReflectError>`: always relative to a namespace the caller holds, never a global lookup |
| `get(value, key)` | `Result<dynamic, ReflectError>` ([01](component-worlds/01-paths-and-values.md#fallible-lookup-on-dynamic)) |
| `set(value, key, new)` | `Result<(), ReflectError>` |
| `call(callable, args)` | `Result<dynamic, ReflectError>` |
| `ty.construct(fields: dynamic)` | `Result<dynamic, ReflectError>`: a record from an object of field values, or `ty.variant(name, payload)` for an enum |
| `cast<T>(value: dynamic)` | `Result<T, ReflectError>`: exact type check, structural for anonymous records |

- **Same behaviour as the language.** Each operation does what `.`, `[key]`, a call
  or a literal would do, using the same paths and inline caches; only failures
  become values.
- **Privacy is respected.** Reflection sees what `.` sees from the calling code.
- **Runtime types never become static types.** There are no types that depend on
  runtime values. `cast<T>` and `std::object::view<T>` are the checked bridges to
  static code.

`ReflectError` is an open enum; this draft adds variants to the two in 01:

```cyallo
pub open enum ReflectError {
    Missing: record { key: string },
    NotObject,
    ReadOnly: record { key: string },                         // e.g. a frozen namespace
    TypeMismatch: record { expected: Type, found: Type },
    NonConforming: record { trait_name: string, member: string },
    TypeQuota,
}
```

### Building types at runtime

The same `std::meta::TypeBuilder` runs at runtime. `build()` then returns
`Result<std::reflect::Type, ReflectError>`, and values of the type are reachable
only through `dynamic`:

```cyallo
let row = std::meta::TypeBuilder::record("Row")
    .field("id", i64)
    .method("label", proc(self: dynamic) -> string { return "row" })
    .implement(Labeled, object { label: proc(self: dynamic) -> string { return "row" } })
    .build()?

let value = row.construct(object { id: 7 })?
let labeled = std::reflect::cast<dyn Labeled>(value)?    // static code calls through the trait
```

- **Behaviour is attached, not compiled.** `.method` attaches existing `fn`/`proc`
  values and closures, which are already compiled. Receivers are `dynamic`, so calls
  use the ordinary dynamic call path. Compiling new code remains the job of
  [recipes](component-worlds/06-components.md#recipes).
- **Traits bridge to static code.** `.implement(Trait, members)` checks the members
  against the trait's signatures when `build()` runs (`NonConforming` otherwise)
  and creates a dispatch table. Values erase to `dyn Trait`, and the existing
  trait-call cache treats the implementation like any other.
- **Lifetime.** A runtime type is reclaimed when no value, descriptor or trait
  object refers to it.

### Why runtime building stays cheap and rare

1. **Interned structural types.** Anonymous types built at runtime go through a
   Session-owned table, so rebuilding the same shape costs a lookup and never
   creates a new shape.
2. **Named builds are explicit.** A named type gets a new identity on every
   `build()`, matching the rule that two `record Foo {}` declarations differ. That is
   the case worth discouraging, and it is the one the checks below target.
3. **`dynamic` only.** No static code depends on a runtime-built type, so building
   one never invalidates or recompiles anything. Only sites that see many such
   types go megamorphic, within the existing four-case cache bound.
4. **Visible.**
   - The checker warns when `build()` outside `const` evaluation is reachable inside
     a loop.
   - `--jit-trace` and `Session` statistics count types built at runtime.
   - A Session quota returns `ReflectError::TypeQuota` once exceeded.
5. **Nothing for everyone else.** Descriptors are materialized lazily, so code that
   never reflects pays no time cost, only the metadata already kept in the Program.

## Costs and risks

- Type functions run during type preparation, so `const` evaluation must work before
  contracts are frozen. That reorders part of the front end.
- Unrolling multiplies code size per specialization; limits keep it bounded.
- Runtime descriptors for compiled records and enums need their names and field
  lists kept in Program metadata.
- Checked-module caching must key type-function results by their arguments'
  contracts.

## Verification

Cyallo examples under `examples/types/` for type functions, `const for`
serialization, runtime inspection and resolution, and a runtime-built type
implementing a trait called from static code. Rejected programs: a `type` value
reaching runtime, a type-function cycle, unrolling past its limit, `value.[name]`
with an unknown or non-constant name, `std::meta` reading another package's private
field, and `cast` to a different nominal type with equal fields. Exercise eager and
deferred checking, interpreter and native engines, and GC stress with runtime-built
types reclaimed while trait objects are live.

## Open questions

- Spelling of compile-time field access (`value.[name]`) and of `const if`.
- Whether `std::meta::TypeBuilder` can build enums and anonymous enums at runtime,
  or records only.
- Whether runtime-built named types should be comparable across Sessions, for
  example when exchanging values between Sessions that built the same type.
- Whether `descriptor<T>()` and `type_of` should share one descriptor table with
  object shapes.
