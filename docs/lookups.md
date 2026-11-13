# Storage paths and user-defined services

`std::lookup::mapped(receiver, key, mapper)` caches a relative storage recipe with
version guards. A state-reading mapper is an ordinary `proc`, checked for readonly
behavior before entry. A pure `fn` is also accepted. On a miss it executes as an
ordinary VM frame, preserving budgets, pauses, errors, and GC. On a hit the runtime
validates dependencies and applies the recipe to the current receiver.

## Storage contracts

| API | Meaning |
| --- | --- |
| `std::cell::Cell::new(value)` | Owned interior-mutable cell |
| `*cell` | Copy the contents; requires Copy |
| `*cell = value` | Replace the contents and drop the old value |
| `cell.replace(value)` | Replace the contents and return the old owned value |
| `@gc Cell<T>` | Explicitly shared traced cell |
| `std::collections::Vec<@gc Cell<T>>` | Growable collection of shared cells |
| `std::lookup::field(&owner, "field")` | Strong retained field `Location<T>` |
| `Location<T>.load()` / `.store(value)` | Read / replace the retained field |
| `std::lookup::path(receiver)` | Start a relative `StoragePath<Receiver>` |
| `StoragePath<R>.field("name")` | Append a statically checked field projection |
| `StoragePath<Vec<T>>.element(index)` | Append a checked collection index |
| `StoragePath<@gc T>.target()` | Follow the managed owner |
| `StoragePath<Cell<T>>.target()` | Read the cell contents |
| `std::lookup::missing<T>()` | Negative `StoragePath<T>` |
| `std::lookup::mapped(receiver, key, mapper)` | Resolve the current `Option<T>` |
| `std::option::some(value)` / `std::option::none<T>()` | Construct optional values |
| `std::any::erase(value)` / `std::any::checked<T>(value)` | Owned erasure / exact-type recovery |

A retained `Location<T>` refers to a field, not a movable collection element.
A collection path rechecks its index each time it is applied. To share an element
across vector growth, store an explicit managed owner in the vector.

```text
record Directory { values: Cell<Vec<@gc Cell<i64>>> }
proc route(root: @gc Directory, key: i64) -> std::lookup::StoragePath<@gc Cell<i64>> {
    return std::lookup::path(root).target().field("values").target().element(key)
}
let root = @gc Directory { values: Cell::new(Vec[@gc Cell::new(42)]) }
let found = std::lookup::mapped(root, 0, route).unwrap()
std::io::print(**found)
```

## Guards and native execution

Mapper reads of versioned cells register dependencies. Mutating a cell or a child
field updates the corresponding version. Snapshot reads use an aggregate guard,
so a large immutable directory needs one warm version check. Nested lookups
propagate both mapper dependencies and result reads into an enclosing mapper.
Writes, I/O, mutable globals, nondeterminism, and unchecked procedure-value calls
are rejected before mapper effects execute. Readonly procedures may call other
checked readonly procedures.

Each site holds at most four receiver/key/mapper cases. Additional cases use the
checked miss path without growing the cache. A version mismatch replaces a stale
case. Cached paths and weak dependency identities do not retain provider instances.
Compaction repairs dependency indices and drops cases with dead dependencies.
The REPL clears caches and abandoned mapper continuations after accepted edits.

The interpreter, native Winch, and both Cranelift tiers share these rules and the
runtime lookup helper. `Session::lookup_stats()` reports hits, native hits,
misses, mapper calls, guard checks, invalidations, and maximum case count.
Native code does not infer an intrinsic from a method's source name.

## A registry implemented in source

Run [lookup-services/main.ya](../examples/lookup-services/main.ya):

```sh
cargo run -- run examples/lookup-services/main.ya --engine cranelift_speed --gc-stress
```

The [registry](../examples/lookup-services/registry.ya) has an explicit GC-owned
`Directory` containing owned cells holding immutable GC snapshots of its key list
and entry vector. Each entry is an `@gc Cell<@gc Binding>`. The mapper reads the key directory and returns a path to
that entry's managed owner. A service handle keeps that owner, then loads its
current provider on each acquisition. Replacement preserves the entry identity.
A generation check prevents an old lease from disposing a replacement.

The example uses ordinary records, generic routines, trait dispatch and lookup
operations. It emits no privileged service opcodes. The separate
[service-context example](../examples/services/main.ya) demonstrates admission,
isolation and dependent cleanup using `std::service`.
