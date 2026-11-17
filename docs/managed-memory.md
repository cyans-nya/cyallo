# Managed ownership and safe allocators

`gc T` is a traced reference. `rc T` is a counted owner. Both are local to one
coroutine by default; aliases to a local object stay in that coroutine.
Allocate them with `gc expression` and `rc expression`. Managed syntax enables
collector integration automatically. A managed reference's identity and payload
survive collection, instruction pauses, and interpreter/native handoffs.

## Storage and locality syntax

| Type | Storage | Locality |
| --- | --- | --- |
| `gc T` or `@local gc T` | Traced | Local (default) |
| `rc T` or `@local rc T` | Reference counted | Local (default) |
| `@shared gc T` | Traced | Shared; runtime support pending |
| `@shared rc T` | Reference counted | Shared; runtime support pending |

The same prefixes apply to construction:

```text
let state: gc Cell<i64> = @local gc Cell::new(0)
let owner: @local rc i64 = rc 42
```

Storage keywords have no `@` sigil. The old `@gc` and `@rc` spellings are rejected
with a migration diagnostic. A value has at most one locality modifier, placed
before its storage keyword. `gc::clone` and `rc::clone` remain ordinary module
paths resolved through standard source declarations.

The parser and type model preserve `@shared` separately from `gc`/`rc`.
Compilation currently rejects shared managed types and allocations with E179;
it does not allocate local storage under a shared annotation or implicitly
promote a local owner. Whole-graph uniqueness, shared tracing/RC, and guarded
shared mutation remain pending. Local does not imply unique. See the
[concurrency tracker](concurrency-language-progress.md).

The [local managed example](../examples/concurrency/ownership/local-managed.ya)
exercises both explicit and default locality. Local managed values cannot be
captured by a new task or sent through a channel, including via aggregates.

## Local managed ownership

Dereference is postfix. Each `.*` removes one reference, managed-owner, or Cell
layer; it has the same precedence as field access, indexing, and calls.

```text
let mut number = 6
let view = &mut number
view.* += 1
let cells = [gc Cell::new(10)]
cells[0].*.* = 12
std::io::print(view.* * 3, ":", cells[0].*.*)
```

Use `view.*.field` or `view.*[i]` to access fields or elements after
dereferencing, and `get().*` to read a returned reference. Parenthesize a
prefix construction before dereferencing its result, as in `(gc 42).*`.
Borrowing remains prefix: `&value.*` borrows the dereferenced place, while
`(&value).*` dereferences the borrow. Existing read-only and Cell restrictions
still apply. The former prefix `*value` syntax is rejected with E180.

Run the [postfix dereference example](../examples/postfix-dereference.ya) to
exercise mutable references, GC cells, tuple projections, and borrowed RC owners.

An RC assignment moves ownership. `std::rc::clone(&owner)` explicitly creates a
new counted owner. `std::rc::downgrade(&owner)` creates a weak owner, and upgrading
it returns `Option<rc T>`. Destruction occurs at the last strong release; allocator
state remains available until the last weak release. Strong cycles are not
collected. Temporary owners and owners on loop exits release their counts.

`Cell<T>` owns its contents. Aliased local interior mutation uses `gc Cell<T>`.
`std::mem::clone(&value)` performs structural cloning and increments nested RC
owners. `Allocation` and aggregates containing it are move-only.

Cell reads use `cell.*` and require Copy contents. Assignment replaces the
contents and drops the old value; it works through a shared reference and an
immutable binding. A Cell never exposes `&T` or `&mut T` into its contents, so
compound assignments such as `cell.* += 1` are rejected. Write `cell.* = cell.* + 1`.
Ordinary references and managed owners each add their own dereference layer:
`shared.*.*` accesses the contents of an `gc Cell<i64>`.

For non-Copy contents, `cell.replace(value)` moves the old contents out and leaves
the supplied value in the cell. There is no implicit clone or default replacement.
An optional owner can be extracted by replacing it with an empty option:

```text
let cell = Cell::new(Option::Some(Vec[1,2]))
let inner = cell.replace(std::option::none<Vec<i64>>()).unwrap()
std::io::print(inner[0], ":", inner[1])
```

For aliased immutable snapshots within a coroutine, store a Copy GC handle in the cell, such as
`Cell<gc Vec<T>>`. Reading the handle retains the same snapshot. Clone the vector
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

Implement `std::mem::Allocator` on an ordinary record. `rc(allocator) value`
moves the allocator state into the control block. `rc(&mut allocator) value`
borrows it, and the owner and its weak references must stay within that borrow.
The source allocator's concrete type is erased from `rc T`; its resolved
allocation/deallocation methods remain in the control block's vtable.

`std::rc::try_new_in(value, allocator)` returns `Result<rc T, AllocError>`.
The `rc(allocator)` expression reports an allocation failure as a checked runtime
error. Failure drops the moved payload. Explicit allocator lifetime annotations
use `rc<'a> T` and `std::rc::Weak<'a, T>`.

| Operation | Contract |
| --- | --- |
| `std::mem::Layout::new(size, align)` | Checked `Result<Layout, AllocError>`; arguments are `usize` |
| `layout.size()`, `layout.align()` | Size and alignment as `usize` |
| `std::mem::allocate(layout)` | Fallible aligned backing allocation |
| `std::mem::deallocate(allocation)` | Consume and release backing allocation |
| `std::mem::AllocError::new(message)` | Construct an allocation error |
| `Result::Ok(value)`, `Result::Err(error)` | Construct a typed result (the enum constructors; no separate functions) |

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
then [gc-rc.ya](../examples/gc-rc.ya) for aliased local GC state inside RC allocations.
[managed.ya](../examples/managed.ya) demonstrates a custom allocator.
