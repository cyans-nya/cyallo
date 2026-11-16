# Data enums and anonymous records — design draft

Status: proposal, 2026-11-16. Nothing here is implemented. Cyallo has no enum
declarations, no `match` and no anonymous record types today: `Option` and `Result`
are intrinsic records, and statuses and failures such as `TaskOutcome`,
`SendError`, `ViewError` and `LoadError` are opaque records queried through
predicates (`is_failed()`, `is_full()`). Syntax is illustrative. Computing types at
compile time and runtime builds on this draft; see
[types as values](types-as-values-draft.md).

## Problem

Predicate-style records are the language-level version of what the
[compiler and runtime rules](compiler-and-runtime-rules.md) forbid inside the
implementation: a kind encoded as a set of booleans.

- **Not exhaustive.** Nothing tells a caller that it forgot `is_stopped()`, or that
  a new case was added.
- **Data tied to the wrong state.** An accessor like `key()` is meaningful only for
  some cases, but callable for all.
- **No single place to read the alternatives.** The set of cases lives in method
  names and documentation, not in a declaration.

A data enum states the alternatives, gives each its own payload, and lets the
checker prove a `match` handles all of them. Payloads often want a few named fields
without declaring a type for each, which needs anonymous records.

## Anonymous records

`record { ... }` is a type expression and a literal, alongside the existing
`object { ... }`:

```cyallo
let r = record { min: 1, max: 9 }                 // literal
fn bounds(values: &[]i64) -> record { min: i64, max: i64 } { ... }
```

| | `record Name { ... }` declaration | `record { ... }` | `object { ... }` |
| --- | --- | --- | --- |
| Identity | Nominal: its declaration | Structural: field names and types | Structural type, shared object identity |
| Semantics | Value | Value | Shared reference |
| Private fields | Allowed | None: all fields public | None |
| Methods, trait impls | Allowed | None | None |

- **Structural identity.** Two anonymous record types are the same type when they
  have the same field names with the same types, like tuples. Field order does not
  matter.
- **Declarations are nominal.** `record Foo {}` and another `record Foo {}` are
  different types, even with equal fields, as today.
- **Naming a structural type does not make it nominal.** `const Foo = record {}` and
  `type Foo = record {}` bind a name to the structural type, so `Foo` equals
  `record {}` and any other binding of the same shape. A name that needs its own
  identity is a declaration.
- Field access, record update and patterns work as for declared records.
  Equality is structural over the fields, as for declared records.

## Enum declarations

Every variant is `Name: Type`, or a bare `Name` for a unit variant (short for
`Name: ()`). The payload type can be anything: a primitive, a tuple, an anonymous
record, a declared record, another enum.

```cyallo
pub enum Shape {
    Point,                                        // unit
    Circle: f64,
    Segment: (Point2, Point2),
    Rect: record { width: f64, height: f64 },
    Labeled: Labeled,                             // a declared record
}

pub enum Option<T> {
    Some: T,
    None,
}
```

- Variants live in the enum's namespace: `Shape::Circle`. `use Shape::{Circle, Rect}`
  imports them like any declaration.
- Each variant is a declaration with its own identity, so aliases, reexports and
  shadowing follow ordinary resolution. Variant names are PascalCase, which keeps
  them apart from bindings in patterns.
- Enums are nominal declarations. They take generic parameters, `impl` blocks and
  trait implementations like records.

## Construction

Every non-unit variant is a constructor function taking its payload:

```cyallo
let a = Shape::Point
let b = Shape::Circle(1.5)                        // Shape::Circle: fn(f64) -> Shape
let c = Shape::Segment((p, q))
let d = Shape::Rect(record { width: 2.0, height: 3.0 })
let e = Shape::Rect { width: 2.0, height: 3.0 }   // short form for a record payload
```

When the payload is a record type, anonymous or declared, `Variant { fields }` is
short for `Variant(PayloadType { fields })`.

## `match`

```cyallo
let area = match shape {
    Shape::Point => 0.0,
    Shape::Circle(r) => 3.14159 * r * r,
    Shape::Segment(_) => 0.0,
    Shape::Rect { width, height } => width * height,   // destructure the payload
    Shape::Labeled(label) => area_of(&label.shape),    // or bind it whole
}
```

- `match` is an expression. An arm is `pattern => expression` or
  `pattern => { statements }`; arms are tried top to bottom.
- **Exhaustive.** Missing a variant is a compile error that names the missing
  variants. `_` matches anything.
- **Patterns:** `Variant(pattern)` matches the payload against a nested pattern;
  `Variant { fields }` destructures a record payload, with `..` for the remaining
  fields; also bindings, `_`, tuple patterns, literals of primitive types,
  or-patterns `A | B` (each side must bind the same names with the same types), and
  guards `pattern if condition`. A guarded arm does not count toward exhaustiveness.

A single-pattern test binds into its branch:

```cyallo
if let Shape::Circle(r) = shape {
    std::io::print("radius ", r)
} else {
    std::io::print("not a circle")
}
```

## Ownership

Matching follows the ownership of the scrutinee:

| Scrutinee | Bindings |
| --- | --- |
| A value (`match shape`) | Own their parts; the scrutinee is consumed unless no arm binds a non-copy part |
| A shared reference (`match &shape`) or a `read` value | Shared references to the parts |
| A mutable reference (`match &mut shape`) | Mutable references to the parts |
| Managed storage (`match shape.*`) | As for a shared reference; payloads keep their `gc`/`rc` storage and locality |

Bindings obey the usual capture, sharing and uniqueness checks. A binding cannot
outlive a borrowed scrutinee, and a moved payload cannot be used through the
original value again.

## Open enums and versioning

An enum that will gain variants is declared `open`:

```cyallo
pub open enum LoadError {
    NotFound: record { path: string },
    Denied: record { path: string },
    // ...
}
```

Outside the package that declares it, a `match` on an open enum must include `_`.
Inside its own package it is checked exhaustively as usual.

| Change | Closed enum | Open enum |
| --- | --- | --- |
| Add a variant | Major | Minor; the variant carries `#[since]` |
| Remove or rename a variant | Major | Major |
| Change a variant's payload type | Major | Major |

This is the enum row of the semver rules in
[component worlds](component-worlds/04-versioning-and-loading.md#semver-for-sdks).

## Typed pipeline

Enums and anonymous records follow the
[language pipeline](compiler-and-runtime-rules.md#builtins-follow-the-language-pipeline):

- Typed IR carries the variant's declaration identity and payload layout, never a
  name or a bare integer tag. Discriminant values exist only at encoding boundaries
  (native code, serialized artifacts) and are validated when decoded.
- An anonymous record type is canonicalized by its sorted field names and field
  types, so equal shapes resolve to one type within a compilation. Its layout is
  chosen by the compiler and is not observable.
- Exhaustiveness is checked against the resolved variant set of the instantiated
  enum type, including through generic substitution.
- Backends may choose compact layouts, for example `Option<gc T>` as a nullable
  pointer. A layout is never observable.
- Standard enums keep their intrinsic identity through `#[cyallo_internal("...")]` on
  the declaration and its variants, as records do today.
- An enum or record value erased to `dynamic` is opaque. `std::object::view` does
  not project enums or records.

## Migrating std

| Today | As an enum |
| --- | --- |
| `record Option<T>` with `some`/`none` | `enum Option<T> { Some: T, None }` |
| `record Result<T, E>` with `ok`/`err` | `enum Result<T, E> { Ok: T, Err: E }` |
| `record TaskOutcome<T>`, `is_completed`/`is_failed`/`is_cancelled`/`is_stopped` | `enum TaskOutcome<T> { Completed: T, Failed: TaskFailure, Cancelled, Stopped }` |
| `record SendError<T>`, `is_closed`/`is_full`/`is_limit`, `into_value` | `enum SendError<T> { Closed: T, Full: T, Limit: T }`: each case keeps the unsent value |
| `record ViewError`, `message` | `open enum ViewError { Missing: record { field: string }, Incompatible: record { field: string, expected: TypeDescriptor } }` |
| `record LoadError`, `is_*` predicates | `open enum LoadError`, see [component worlds](component-worlds/04-versioning-and-loading.md#errors) |

Existing methods (`is_some()`, `unwrap()`, `is_ok()`, `message()`) remain as
conveniences implemented on the enum, so current code keeps working. New std
statuses and errors are enums from the start.

## Verification

Per [completion rules](compiler-and-runtime-rules.md#completion-is-verified-in-cyallo):
Cyallo examples under `examples/enums/`, and rejected programs for a non-exhaustive
`match`, a wrong payload shape, use after a moving match, or-patterns that bind
differently, a missing `_` on a foreign open enum, a binding that outlives a
borrowed scrutinee, and a declared record used where an equal anonymous record is
expected. Exercise eager and deferred checking, interpreter and native engines, and
GC stress with managed payloads.

## Open questions

- `let ... else` for early exit on a failed pattern.
- The `?` operator: with `Result` an enum, `expr?` can be defined as a `match` that
  returns `Err(e)` early. Whether it converts between error types, and how, is
  undecided. The [component worlds](component-worlds/README.md) draft uses `?`
  assuming identical error types.
- Explicit discriminant values for native interop and serialization.
- Anonymous enums (`fn f() -> enum { A, B }`), which raise structural sum-type
  questions this draft avoids.
