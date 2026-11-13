# Managed ownership and safe allocators

`@gc T` is a traced shared reference. `@rc T` is a session-local counted owner.
Allocate them with `@gc expression` and `@rc expression`. Managed syntax enables
collector integration automatically. A managed reference's identity and payload
survive collection, instruction pauses, and interpreter/native handoffs.

An RC assignment moves ownership. `std::rc::clone(&owner)` explicitly creates a
new counted owner. `std::rc::downgrade(&owner)` creates a weak owner, and upgrading
it returns `Option<@rc T>`. Destruction occurs at the last strong release; allocator
state remains available until the last weak release. Strong cycles are not
collected. Temporary owners and owners on loop exits release their counts.

`Cell<T>` owns its contents. Shared interior mutation is explicit with `@gc Cell<T>`.
`std::mem::clone(&value)` performs structural cloning and increments nested RC
owners. `Allocation` and aggregates containing it are move-only.

Cell reads use `*cell` and require Copy contents. Assignment replaces the
contents and drops the old value; it works through a shared reference and an
immutable binding. A Cell never exposes `&T` or `&mut T` into its contents, so
compound assignments such as `*cell += 1` are rejected. Write `*cell = *cell + 1`.
Ordinary references and managed owners each add their own dereference layer:
`**shared` accesses the contents of an `@gc Cell<i64>`.

For non-Copy contents, `cell.replace(value)` moves the old contents out and leaves
the supplied value in the cell. There is no implicit clone or default replacement.
An optional owner can be extracted by replacing it with an empty option:

```text
let cell = Cell::new(std::option::some(Vec[1,2]))
let inner = cell.replace(std::option::none<Vec<i64>>()).unwrap()
std::io::print(inner[0], ":", inner[1])
```

For shared immutable snapshots, store a Copy GC handle in the cell, such as
`Cell<@gc Vec<T>>`. Reading the handle retains the same snapshot. Clone the vector
explicitly before editing it, then replace the handle with a new GC allocation.

## Circular references

Recursive records may contain managed references or growable collections of
their own type. Direct recursive value layouts, including recursion through an
owned Cell or fixed array alone, are rejected because they require infinite
storage. Resolution retains nominal type identities at recursive edges.

[gc-cycles.ya](../examples/gc-cycles.ya) connects two nodes using strong GC
references in both directions. Traversal prints `10 -> 20 -> 10`. Once the
procedure returns, neither node is reachable from a root, and the collector
reclaims the cycle. No weak edge is needed. RC ownership still needs weak
backedges to prevent strong reference cycles from retaining each other.

## Allocator protocol

The standard trait has these checked signatures:

```text
use std::mem::{Layout, Allocation, AllocError}
trait Allocator {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError>
    proc deallocate(self: &mut Self, allocation: Allocation)
}
```

Implement `std::mem::Allocator` on an ordinary record. `@rc(allocator) value`
moves the allocator state into the control block. `@rc(&mut allocator) value`
borrows it, and the owner and its weak references must stay within that borrow.
The source allocator's concrete type is erased from `@rc T`; its resolved
allocation/deallocation methods remain in the control block's vtable.

`std::rc::try_new_in(value, allocator)` returns `Result<@rc T, AllocError>`.
The `@rc(allocator)` expression reports an allocation failure as a checked runtime
error. Failure drops the moved payload. Explicit allocator lifetime annotations
use `@rc<'a> T` and `std::rc::Weak<'a, T>`.

| Operation | Contract |
| --- | --- |
| `std::mem::Layout::new(size, align)` | Checked `Result<Layout, AllocError>`; arguments are `usize` |
| `layout.size()`, `layout.align()` | Size and alignment as `usize` |
| `std::mem::allocate(layout)` | Fallible aligned backing allocation |
| `std::mem::deallocate(allocation)` | Consume and release backing allocation |
| `std::mem::AllocError::new(message)` | Construct an allocation error |
| `std::result::ok<T,E>(value)`, `std::result::err<T,E>(error)` | Construct a typed result |

An allocation is an opaque owned token; source code has no raw pointer or unsafe
operation. Allocators can move tokens into a pool and return them on later calls.
The runtime validates backing size and alignment before storing a payload
reference. Allocation budgets are bounded by the Session's storage limit.

The current managed representation is indirect: the supplied backing block stores
the relocatable payload descriptor actually used by RC access. The collector
registers and repairs that descriptor in place. Backing storage is not an unused
accounting token. Its root registration ends before the deallocation method runs.

Deallocation uses ordinary procedure frames after the releasing instruction
commits. It can pause or reenter language execution without retaining a native
stack pointer. All tiers publish successor liveness before entering cleanup.
Generation-checked owner tokens allow metadata reuse without confusing obsolete
VM snapshots with new owners. REPL edits relink deallocation method identities.

Pure functions may use effectless references to RC values, but cannot own,
construct, clone, observe counts, or destroy effectful lifecycle values, including
through aggregates or captures. Start with [gc-sharing.ya](../examples/gc-sharing.ya)
and [rc-ownership.ya](../examples/rc-ownership.ya) for the individual ownership models,
then [gc-rc.ya](../examples/gc-rc.ya) for shared GC state inside RC allocations.
[managed.ya](../examples/managed.ya) demonstrates a custom allocator.
