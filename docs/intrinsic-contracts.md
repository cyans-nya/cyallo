# Intrinsic effect contracts

Source of truth: [`crates/cyallo-ir/src/contract.rs`](../crates/cyallo-ir/src/contract.rs).
Every engine intrinsic (the 100 variants of `Intrinsic`) has one **contract** returned by
`Intrinsic::contract()`. A contract states what the operation *can* do, independent of
how it is lowered today. The compiler, optimizer and backends read the contract instead
of keeping their own lists. This page defines each field precisely, says what a consumer
may conclude from it, and lists every intrinsic's current contract.

Contracts are about semantics, not today's code generation. A scalar cast is `Never` at
a safepoint even though it currently runs through the interpreter helper.

## Reading a contract

```text
IntrinsicContract {
    disposition,          // where the operation should live
    safepoint,            // can it collect, or run guest code / park?
    effects: Effects { ... },   // what it reads, writes, allocates, observes
    consumes_arguments,   // are operands moved in?
}
```

### Effects

Each flag is a **may**: `true` means the operation can do this on some input; `false`
is a promise that it never does. A consumer may rely on `false`; it must assume `true`
may happen.

| Flag | `true` means | A `false` lets an optimizer… |
| --- | --- | --- |
| `reads_memory` | Reads heap, managed or operand storage beyond its scalar operands | …treat the call as independent of memory writes (reorder across stores) |
| `writes_memory` | Mutates memory visible to the program (including operand storage and allocator state) | …keep values cached across the call |
| `allocates` | May allocate (and so may exhaust resources) | …know no allocation (and no collection) happens |
| `fresh_identity` | Result has a new identity: cell, channel, clock, symbol, service key | …treat equal operands as giving equal, shareable results |
| `nondeterministic` | Result can differ between runs or calls with equal operands (randomness, scheduling) | …fold it at compile time or merge equal calls |
| `io` | Observable effect outside the program (output, input, loading) | …remove or reorder it relative to other effects |
| `may_fail` | Can stop the caller with a runtime failure (a trap, not a returned `Result`) | …drop an unused call without changing failure behavior |
| `may_park` | Can suspend the coroutine and resume later | …assume it runs to completion without yielding to the scheduler |
| `calls_guest` | Can run guest code: callbacks, destructors, allocators | …assume no user code runs and no re-entry happens |

`may_fail` is the *failure* effect only. A `Result` returned as a value is ordinary
data and not a failure. Where an operation can fail on overflow or domain errors the
contract is `may_fail` conservatively; tightening it per operation is future work.

**Derived: `Effects::is_pure()`** is true when none of `writes_memory`, `fresh_identity`,
`nondeterministic`, `io`, `may_park`, `calls_guest` is set. A pure operation is observable
only through its result: it may read memory, allocate and fail, but it can be folded
when its operands are constant, removed when its result is unused (unless `may_fail`
must be preserved), and called from a `fn`.

### Safepoint

| Value | Meaning | Consequence |
| --- | --- | --- |
| `Never` | Cannot collect, run guest code or park | Backends may emit it inline between checks; no GC root map or resume point is needed at it |
| `MayCollect` | May allocate and so trigger a collection, but runs no guest code | All live managed values must be rooted at it; moving collection may relocate them |
| `MayReenter` | May run guest code or park the coroutine | All state must be rooted **and resumable**: the frame can be suspended and other code can run before it returns |

Ordering is `Never < MayCollect < MayReenter`. Invariants (enforced by tests):
`may_park` or `calls_guest` implies `MayReenter`; `allocates` implies not `Never`.

**Derived: `IntrinsicContract::inlinable()`** is true for `Never` safepoints that do not
park or call guest code. It is the gate a backend uses before emitting the operation
inline; today no backend inlines an intrinsic, and the gate is used for the safepoint
metadata below.

### Disposition

| Value | Meaning | Direction |
| --- | --- | --- |
| `EnginePrimitive` | Needs engine knowledge of values, memory or the GC | Stays engine-owned; backends add lowerings and fast paths |
| `SourceCandidate` | Pure computation expressible as Cyallo source over primitives | Moves into SDK source bodies |
| `HostCall` | Crosses to the host: I/O, scheduler, clock, loader | An opaque call with declared effects and the Pending protocol |

### Consumes

`consumes_arguments`: operands are moved into the operation rather than borrowed. The
ownership checker reads it through `Intrinsic::consumes_argument(index)` (one flag for all
operands today).

## Who reads contracts today

| Consumer | Reads | Effect |
| --- | --- | --- |
| `Intrinsic::consumes_argument` (ownership/declaration checks) | `consumes_arguments` | Replaced the hand-written list; identical results |
| `builtins::pure` (constant folding in `src/constants.rs`, purity in `effects.rs`) | `effects.is_pure()` | Same answers for everything that lowers to `Op::Builtin` |
| `effects.rs` for `Op::Builtin` | `may_fail`, `allocates`, `io`, purity | A total scalar builtin is no longer `may_fail`/`may_allocate`; allocation still counts as a possible failure |
| `gc_metadata.rs` for `Op::Builtin` | `safepoint` | `Never` builtins are not `may_collect`/`may_reenter`: no OSR liveness checkpoint, no post-call re-entry check |
| `effects.rs` and `gc_metadata.rs` for `Op::Lookup` | `lookup::Operation::contract()` (below) | `may_allocate`/`may_fail` and the safepoint come from the operation's origin contract; a lookup that cannot collect is no longer a collection or re-entry point |
| `declarations.rs::operand_mode` for member declarations | `MemberContract::operands` | Replaced the hand-written operand table; identical for all 75 members and operand positions 0-4 |

Member intrinsics (below) have contracts, and `operand_mode` in `declarations.rs` now reads
them. Not yet consulted: `lookup::Operation::writes`, the intrinsic match in `ownership.rs`,
the effects/GC metadata for `Op::Service` and `Op::Dynamic`, and backend inlining.

## Rules for changing a contract

1. The contract must be true for **every input**. A contract that is too strong is a
   miscompilation: the optimizer trusts `false` flags and `Never`.
2. When unsure, set the flag. A conservative contract costs speed; an unsound one costs
   correctness.
3. The `match` in `Intrinsic::contract` has no wildcard, so adding an intrinsic forces a
   decision. Add a row, then run `cargo test -p cyallo-ir contract`.
4. The tests in `contract.rs` check: ownership equals the previous hand-written list; no
   intrinsic is pure in its contract but impure in the old rule; internal consistency
   (above); the audited partition by disposition; and the first native lowering targets
   stay `inlinable`.
5. A change that moves an intrinsic between `Never` and `MayCollect` affects GC metadata,
   OSR checkpoints and backends; run the GC-stress and engine suites.

## Embedder and SDK contracts

The same record is the contract an embedder-defined intrinsic or a host call must declare
when it is registered (planned). The runtime validates it against the SDK item's
signature, and the optimizer trusts only what was validated. An embedder intrinsic must
also supply reference semantics as a Cyallo body or declarative IR; the contract
describes that reference, and a native lowering that does more than its contract is a
bug the conformance harness is meant to catch.

## Current contracts

Generated from `Intrinsic::contract()` on 2026-11-17 (100 intrinsics). `Effects` lists
the flags that are set; `-` means none. If this table and `contract.rs` disagree, the
code is right.

### Engine primitives (70)

| Intrinsic | Safepoint | Effects | Consumes | Purity |
| --- | --- | --- | --- | --- |
| `ObjectView` | MayCollect | reads, allocates | no | pure |
| `ObjectFreeze` | Never | reads, writes | no | impure |
| `LayoutNew` | MayCollect | allocates, fails | no | pure |
| `Allocate` | MayCollect | writes, allocates, fails | yes | impure |
| `Deallocate` | MayCollect | writes, fails | yes | impure |
| `AllocErrorNew` | MayCollect | allocates, fails | no | pure |
| `ResultOk` | MayCollect | allocates | yes | pure |
| `ResultErr` | MayCollect | allocates | yes | pure |
| `RcTryNewIn` | MayReenter | writes, allocates, guest | yes | impure |
| `Length` | Never | reads | no | pure |
| `Truncate` | Never | fails | no | pure |
| `CharFromCode` | Never | fails | no | pure |
| `Abs` | Never | fails | no | pure |
| `Min` | Never | fails | no | pure |
| `Max` | Never | fails | no | pure |
| `Clamp` | Never | fails | no | pure |
| `NumAbs` | Never | fails | no | pure |
| `NumMin` | Never | fails | no | pure |
| `NumMax` | Never | fails | no | pure |
| `NumClamp` | Never | fails | no | pure |
| `Round` | Never | fails | no | pure |
| `Floor` | Never | fails | no | pure |
| `Ceil` | Never | fails | no | pure |
| `Sqrt` | Never | fails | no | pure |
| `Pow` | Never | fails | no | pure |
| `FloatRound` | Never | fails | no | pure |
| `FloatFloor` | Never | fails | no | pure |
| `FloatCeil` | Never | fails | no | pure |
| `FloatSqrt` | Never | fails | no | pure |
| `FloatPow` | Never | fails | no | pure |
| `CellNew` | MayCollect | allocates, fresh | yes | impure |
| `ArrayEmpty` | MayCollect | allocates | no | pure |
| `ArrayFrom` | MayCollect | reads, allocates | no | pure |
| `VecLiteral` | MayCollect | allocates | yes | pure |
| `DequeLiteral` | MayCollect | allocates | yes | pure |
| `VecFrom` | MayCollect | reads, allocates | no | pure |
| `DequeFrom` | MayCollect | reads, allocates | no | pure |
| `OptionSome` | MayCollect | allocates | yes | pure |
| `OptionNone` | Never | - | no | pure |
| `Erase` | MayCollect | allocates | yes | pure |
| `Checked` | MayCollect | allocates, fails | yes | pure |
| `FreshSymbol` | MayCollect | allocates, fresh | no | impure |
| `LookupPath` | MayCollect | reads, allocates, fails | yes | pure |
| `MissingPath` | MayCollect | reads, allocates | no | pure |
| `MappedLookup` | MayReenter | reads, allocates, fails, guest | no | impure |
| `ContextNew` | MayCollect | allocates, fresh | no | impure |
| `ServiceKeyNew` | MayCollect | allocates, fresh | no | impure |
| `CastI8` | Never | fails | no | pure |
| `CastI16` | Never | fails | no | pure |
| `CastI32` | Never | fails | no | pure |
| `CastI64` | Never | fails | no | pure |
| `CastU8` | Never | fails | no | pure |
| `CastU16` | Never | fails | no | pure |
| `CastU32` | Never | fails | no | pure |
| `CastU64` | Never | fails | no | pure |
| `CastIsize` | Never | fails | no | pure |
| `CastUsize` | Never | fails | no | pure |
| `CastF32` | Never | fails | no | pure |
| `CastF64` | Never | fails | no | pure |
| `RcClone` | Never | reads, writes | no | impure |
| `RcDowngrade` | Never | reads, writes | no | impure |
| `RcUpgrade` | Never | reads, writes | no | impure |
| `RcStrongCount` | Never | reads | no | pure |
| `RcWeakCount` | Never | reads | no | pure |
| `GcClone` | MayCollect | reads, allocates | no | pure |
| `DropValue` | MayReenter | writes, fails, guest | yes | impure |
| `CloneValue` | MayCollect | reads, allocates, fails | no | pure |
| `FieldLocation` | MayCollect | reads, allocates | no | pure |
| `ArrayLength` | Never | reads | no | pure |
| `StringLength` | Never | reads | no | pure |

### Source candidates (22)

| Intrinsic | Safepoint | Effects | Consumes | Purity |
| --- | --- | --- | --- | --- |
| `Left` | MayCollect | reads, allocates, fails | no | pure |
| `Right` | MayCollect | reads, allocates, fails | no | pure |
| `Mid` | MayCollect | reads, allocates, fails | no | pure |
| `LowerChar` | Never | - | no | pure |
| `UpperChar` | Never | - | no | pure |
| `CharCode` | Never | - | no | pure |
| `StringFrom` | MayCollect | reads, allocates | no | pure |
| `ParseLegacy` | Never | reads, fails | no | pure |
| `IsNumeric` | Never | reads | no | pure |
| `Trim` | MayCollect | reads, allocates | no | pure |
| `Upper` | MayCollect | reads, allocates | no | pure |
| `Lower` | MayCollect | reads, allocates | no | pure |
| `Contains` | Never | reads | no | pure |
| `StartsWith` | Never | reads | no | pure |
| `EndsWith` | Never | reads | no | pure |
| `Replace` | MayCollect | reads, allocates | no | pure |
| `Keys` | MayCollect | reads, allocates | no | pure |
| `Values` | MayCollect | reads, allocates | no | pure |
| `Count` | Never | reads | no | pure |
| `Sum` | Never | reads, fails | no | pure |
| `Parse` | MayCollect | reads, allocates | no | pure |
| `ArraySum` | Never | reads, fails | no | pure |

### Host calls (8)

| Intrinsic | Safepoint | Effects | Consumes | Purity |
| --- | --- | --- | --- | --- |
| `ModuleLoad` | MayReenter | allocates, nondet, io, fails, parks | no | impure |
| `ChannelBounded` | MayCollect | allocates, fresh, fails | no | impure |
| `ChannelUnbounded` | MayCollect | allocates, fresh, fails | no | impure |
| `ClockNew` | MayCollect | allocates, fresh, fails | no | impure |
| `TaskSpawn` | MayCollect | allocates, fresh, nondet, fails | yes | impure |
| `Random` | Never | nondet, fails | no | impure |
| `Print` | MayReenter | reads, io, fails, parks | no | impure |
| `Input` | MayReenter | writes, io, fails, parks | no | impure |

## Member contracts

`MemberIntrinsic::contract()` (75 members: methods of native types such as channels, tasks,
generators, cells, collections, contexts) returns a `MemberContract`. It uses the same
`Disposition`, `Safepoint` and `Effects` as an intrinsic, with the meanings above, and
replaces `consumes_arguments` with per-operand passing:

| Operand mode | Meaning |
| --- | --- |
| `read` | Copied or borrowed for the call; the caller keeps it |
| `move` | Moved into the operation; the caller no longer owns it |
| `&` | Shared address of the operand's storage |
| `&mut` | Mutable address of the operand's storage |

`OperandModes` gives the receiver (index 0), the mode of the remaining operands, and
positional exceptions, so `CellReplace` is `&` receiver and a moved new value, and
`ChannelSend` reads the channel and moves the value. The ownership checker reads these
modes through `operand_mode`. Dispositions follow the same reasoning as intrinsics: thin
readers and extractors over a value (`Option::is_some`, `Result::unwrap`, `Task::is_failed`)
are source candidates; scheduler-facing members (channels, clocks, task join/cancel) are
host calls; storage, collection, generator, reference-count and service members are engine
primitives. `calls_guest` is set where guest code can run: generator start/resume/close,
`LocationStore` (the old value's destructor), `ContextInject` and the dispose members.
Where a contract is uncertain (slice descriptors, collection growth) it allocates.

Generated from `MemberIntrinsic::contract()` on 2026-11-17. If this table and the code
disagree, the code is right.

### Engine primitives (34)

| Member | Safepoint | Effects | Operands 0, 1, 2 | Purity |
| --- | --- | --- | --- | --- |
| `GeneratorStart` | MayReenter | reads, writes, allocates, fails, guest | &mut, move, move | impure |
| `GeneratorResume` | MayReenter | reads, writes, allocates, fails, guest | &mut, move, move | impure |
| `GeneratorNext` | MayReenter | reads, writes, allocates, fails, guest | &mut, move, move | impure |
| `GeneratorClose` | MayReenter | writes, fails, guest | &mut, move, move | impure |
| `LayoutSize` | Never | reads | read, read, read | pure |
| `LayoutAlign` | Never | reads | read, read, read | pure |
| `RcClone` | Never | reads, writes | read, read, read | impure |
| `RcDowngrade` | Never | reads, writes | read, read, read | impure |
| `RcUpgrade` | Never | reads, writes | read, read, read | impure |
| `RcStrongCount` | Never | reads | read, read, read | pure |
| `RcWeakCount` | Never | reads | read, read, read | pure |
| `GcClone` | MayCollect | reads, allocates | read, read, read | pure |
| `Slice` | MayCollect | reads, allocates | &, read, read | pure |
| `MutableSlice` | MayCollect | reads, allocates | &mut, read, read | pure |
| `CollectionPushBack` | MayCollect | reads, writes, allocates, fails | &mut, move, move | impure |
| `CollectionPushFront` | MayCollect | reads, writes, allocates, fails | &mut, move, move | impure |
| `CollectionPopBack` | MayCollect | reads, writes, allocates | &mut, move, move | impure |
| `CollectionPopFront` | MayCollect | reads, writes, allocates | &mut, move, move | impure |
| `CollectionLength` | Never | reads | &, read, read | pure |
| `CellReplace` | Never | reads, writes | &, move, read | impure |
| `LocationLoad` | MayCollect | reads, allocates, fails | read, read, read | pure |
| `LocationStore` | MayReenter | reads, writes, fails, guest | read, move, read | impure |
| `StoragePathField` | MayCollect | reads, allocates, fails | move, read, read | pure |
| `StoragePathElement` | MayCollect | reads, allocates, fails | move, read, read | pure |
| `StoragePathTarget` | MayCollect | reads, allocates, fails | move, read, read | pure |
| `ContextExtend` | MayCollect | allocates, fresh | read, read, read | impure |
| `ContextIsolate` | MayCollect | allocates, fresh | read, read, read | impure |
| `ContextService` | MayCollect | reads, allocates, fails | read, read, read | pure |
| `ContextProvide` | MayCollect | reads, writes, allocates, fails | read, read, move | impure |
| `ContextAvailable` | Never | reads | read, read, read | pure |
| `ContextDispose` | MayReenter | writes, fails, guest | read, read, read | impure |
| `RegistrationDispose` | MayReenter | writes, fails, guest | read, read, read | impure |
| `ContextInject` | MayReenter | reads, writes, fails, guest | read, read, move | impure |
| `ContextOnDispose` | MayCollect | writes, allocates | read, move, read | impure |

### Source candidates (29)

| Member | Safepoint | Effects | Operands 0, 1, 2 | Purity |
| --- | --- | --- | --- | --- |
| `GeneratorIsComplete` | Never | reads | read, read, read | pure |
| `GeneratorIsYielded` | Never | reads | read, read, read | pure |
| `GeneratorYielded` | Never | reads, fails | move, read, read | pure |
| `GeneratorCompleted` | Never | reads, fails | move, read, read | pure |
| `ResultIsOk` | Never | reads | read, read, read | pure |
| `ResultIsErr` | Never | reads | read, read, read | pure |
| `ResultUnwrap` | Never | reads, fails | move, read, read | pure |
| `ResultUnwrapErr` | Never | reads, fails | move, read, read | pure |
| `RangeReverse` | MayCollect | reads, allocates | read, read, read | pure |
| `OptionIsSome` | Never | reads | read, read, read | pure |
| `OptionIsNone` | Never | reads | read, read, read | pure |
| `OptionUnwrap` | Never | reads, fails | move, read, read | pure |
| `OptionOrFail` | Never | reads, fails | move, read, read | pure |
| `SendErrorValue` | Never | reads, fails | move, read, read | pure |
| `SendErrorClosed` | Never | reads | read, read, read | pure |
| `SendErrorFull` | Never | reads | read, read, read | pure |
| `SendErrorLimit` | Never | reads | read, read, read | pure |
| `LoadErrorNotFound` | Never | reads | read, read, read | pure |
| `LoadErrorDenied` | Never | reads | read, read, read | pure |
| `LoadErrorCheck` | Never | reads | read, read, read | pure |
| `LoadErrorMessage` | MayCollect | reads, allocates | read, read, read | pure |
| `ViewErrorMessage` | MayCollect | reads, allocates | read, read, read | pure |
| `ReceiveErrorClosed` | Never | reads | read, read, read | pure |
| `ReceiveErrorEmpty` | Never | reads | read, read, read | pure |
| `TaskCompleted` | Never | reads, fails | move, read, read | pure |
| `TaskIsCompleted` | Never | reads | read, read, read | pure |
| `TaskIsFailed` | Never | reads | read, read, read | pure |
| `TaskIsCancelled` | Never | reads | read, read, read | pure |
| `TaskIsStopped` | Never | reads | read, read, read | pure |

### Host calls (12)

| Member | Safepoint | Effects | Operands 0, 1, 2 | Purity |
| --- | --- | --- | --- | --- |
| `ChannelClone` | MayCollect | writes, allocates | read, read, read | impure |
| `ChannelClose` | Never | writes, nondet | read, read, read | impure |
| `ChannelIsClosed` | Never | reads, nondet | read, read, read | impure |
| `ChannelSend` | MayReenter | writes, allocates, nondet, fails, parks | read, move, read | impure |
| `ChannelTrySend` | MayCollect | writes, allocates, nondet | read, move, read | impure |
| `ChannelReceive` | MayReenter | writes, allocates, nondet, fails, parks | read, read, read | impure |
| `ChannelTryReceive` | MayCollect | writes, allocates, nondet | read, read, read | impure |
| `ClockClone` | MayCollect | writes, allocates | read, read, read | impure |
| `ClockSleep` | MayReenter | nondet, parks | read, read, read | impure |
| `TaskJoin` | MayReenter | writes, allocates, nondet, fails, parks | move, read, read | impure |
| `TaskCancel` | Never | writes, nondet | read, read, read | impure |
| `TaskIsFinished` | Never | reads, nondet | read, read, read | impure |

## Lowered operations

`Op::Lookup` carries a `lookup::Operation`, not the intrinsic or member that produced it.
`Operation::contract()` (in `contract.rs`) maps each operation to the contract of the
intrinsic or member it implements, and returns `OperationContract::CONSERVATIVE`
(re-enter, read, write, allocate, fail, call guest code) for the operations that have no
single origin: `LayoutForRc`, `ManagedNew`, `Range`, `CellRead`, `CellWrite`,
`StoragePathNew`, `NativeCallable`. The `match` has no wildcard, so a new operation must be
placed. Both routes to an operation (an intrinsic and a member) must agree; a test checks
this for the reference-count and clone operations.

**Conformance check.** Collection in the interpreter starts only when a heap cell is
allocated. In debug builds, `src/runtime.rs` asserts that a lookup whose contract says
`Never` allocates no cell. The whole suite runs with this assertion and it did not fire,
so no operation currently labeled non-collecting allocated through the interpreter. It
checks cell allocation only, not other effects, and it does not cover builtins (the
builtin helper takes no heap access) or service operations.
