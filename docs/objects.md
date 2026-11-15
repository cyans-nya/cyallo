# Open objects and dynamic values

Records and tuples retain fixed value semantics. Objects have shared local
identity and immutable shape descriptors with contiguous property slots.

```cyallo
type Named = object { name: string, .. }
let item = object { name: "Ada", score: 10 }
let named: Named = item
named.name = "Grace"
named.active = true
std::io::print(item.name, ":", named.active)
```

An `object` literal establishes its initial fields as required typed members.
Extra properties added later are configurable. Property names use exact string
spelling. An unknown field of an open type has type `dynamic`. Computed string
keys also return `dynamic` and check membership at runtime.

```cyallo
let bag = dynamic { score: 10 }
bag["score"] = "ten"
std::io::print(delete bag.score)
```

`dynamic { ... }` creates the same storage with configurable initial properties.
Adding/deleting fields or changing a configurable field's representation
transitions to an immutable shape. Missing reads fail without implicit undefined
values. Compound writes evaluate the base and key once, before the right side.

`dynamic` also erases owned primitive, aggregate and callable values. Operators
and calls check actual types. Ordinary numeric rules still apply. Boolean
operators short-circuit. Indexing supports object string keys and checked reads
of one-dimensional arrays, tuples and strings. Mutating an erased aggregate
requires converting it to a checked typed local first.

Object handles copy their identity. Erased values retain owned move semantics;
use `std::mem::clone(&value)` when another owned erased value is needed. Cloning
an erased object retains its identity. Erased move-only resources cannot clone.

`std::object::view<T>(value)` checks a structural `object` type, returns
`Result<T, ViewError>`, and retains the object and extra properties. Nested
requirements are checked before contracts are established. Successful views of
mutable objects enforce required contracts through every alias: retyping or
deleting a required field fails. Conflicting mutable contracts fail.
`std::object::freeze(value)` permanently makes that object readonly through all
aliases. Freezing is shallow; nested objects have their own access state. Loaded
module namespaces freeze their entire exported module tree.

Objects and dynamic values remain local to their coroutine. Borrows cannot be
erased into owned storage, including borrowed closure captures. Property borrows
are currently rejected; references to slots cannot survive shape changes. An
accepting procedure cannot implicitly promote or transport a local object.

`cyallo-shapes` owns property identities, immutable descriptors, field contracts
and lookup plans. Each Session owns its domain and per-site feedback, with at
most four guarded shape/slot cases. Small shapes search IDs directly; larger
shapes binary-search sorted IDs and verify membership. Computed sites guard both
key and shape.

Winch and both Cranelift modes emit native property guards. Supported `i64`,
`f64`, boolean and character reads load directly from a native slot after checking
the receiver, Session-owned shape, key, slot extent and representation. Writes
guard a mutable shape and the input representation, then use a narrow commit
helper to update the canonical value and storage version. Other values, new
fields, representation changes, missing keys and failed guards use the checked
general operation. Object-handle copies also use native slots.

Computed keys have an exact identity guard backed by a retained string; equal
strings with different allocations safely take the content-checked miss path.
Caches retain at most four shapes. Shapes from independent Sessions are never
embedded into shared executables. Native object views are rebuilt after moving
GC and refreshed when contracts or shapes change. No object pointer survives a
collecting helper. PC, fuel, roots and pending work follow the ordinary native
safepoint protocol.

`Session::object_stats()` exposes native read/write hits, guard misses, helper
lookups, shape misses and misses beyond four shapes. Internal verification can
select `ObjectIcMode::Disabled` on a Session to compare the checked fallback with
the generated path. These are runtime verification interfaces; source primitives
still resolve from their annotated declarations.

The reproducible [differential fuzz tests](../tests/object_fuzz.rs) generate 32
seeded programs and compare output, typed failures and instruction counts across
eager/deferred checking, interpreter/Winch/Cranelift, enabled/disabled ICs, GC
stress and single-instruction/137-instruction handoffs. They exercise more than
four shapes, slot shifts, exact/Unicode keys, required/readonly contracts, aliases,
GC/RC graphs and calls. Separate cases cover scalar tags, suspended generators,
independent Session domains and newly compiled published snapshots. Assertions
require both native hits and guard misses to execute. A failure prints its seed
and complete Cyallo source. Run with `cargo test --test object_fuzz`.

Automatic native compilation after publication, direct native stores without
the commit helper, owned/non-scalar property handlers, speculative call
specialization, unboxed canonical fields, perfect hash plans and a public
reflection API remain future work. [Native IC measurements](native-object-ics.md#measured-scope)
describe the measured scope and reproduction commands.

See [examples](../examples/objects/README.md), [runtime loading](dynamic-import.md)
and [verification](../tests/objects.rs).
