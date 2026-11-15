# Moving, compacting GC plan

Status (2026-11-11): the first cell collector is implemented behind `--gc`.
Interpreter, native Winch, and both Cranelift tiers pass ARM64 stress tests,
including one-instruction budget resumptions. Q1–Q7 were explicitly accepted;
the remaining choices were delegated. The design below still includes work
required before default rollout. Language syntax for first-class functions,
composites, and modules remains separate work.

### Implementation checkpoint

- Chunked cell arena, precise mark/compact, root/edge relocation, and release of
  unreachable Rust owners and empty trailing chunks are implemented. Cell
  indices remain `usize` in existing Values, bounded to the 32-bit object range;
  dedicated typed indices and additional object arenas remain migration work.
- Frame/global slots and owned helper intermediates are explicitly registered.
  Generational root-slot IDs reject stale reuse. Every instruction boundary has
  liveness metadata; scalar, owned-value, and location registers are distinct.
  Native emitters route collecting/reentrant operations through materialization
  helpers and rebuild mirrors after movement, without scanning machine stacks.
- INTEGER, REAL, BOOLEAN, and CHAR locals and BYVAL parameters occupy VM frame
  register slots when their addresses are used only for reads, writes, and
  INPUT in the same frame. These values require no cell allocation or root
  registration and survive suspension.
- `GcScope`, `NoGcScope`, and an initial owned `RootedCell` host API enforce
  borrowed access across collection. Public construction currently covers
  integers; general typed object construction remains internal. Sessions and
  host roots are thread confined in this binding; compiler workers use Program
  metadata only.
- Stress tests cover reachable/unreachable cycles, exactly-once destruction,
  dense/sparse copies, aliases, nested helper allocation, dead temporaries,
  INPUT/errors, REPL reset/replacement, and fixed-tier native continuations.
- Collection runs at allocation pressure or the existing cell limit. GC remains
  opt-in: the **64 MiB byte budget is not enforced**, external/shared-buffer
  accounting is incomplete, host allocator exhaustion is not recoverable, and
  native x64 runtime validation remains open. These are default-rollout gates.

Run `cargo test --test gc`, `cargo test --workspace --all-features`, and
`cargo bench --bench gc`. See [initial measurements](gc-benchmarks.md).
Collector ownership tests, registered VM slots/native mirror rebuilding, and
the owned host-root path also pass Miri. The frame-slot test explicitly moves
registered owners and then repeatedly collects; slot storage uses `Rc` plus
`UnsafeCell` so owner moves cannot invalidate published root pointers.

`examples/gc-cells.ya` exercises more than the default million-cell cumulative
allocation limit. An ARM64 Cranelift speed run with `--gc --jit-trace=summary` finished
with output `40000400001:200000`. Frame slots reduced cell allocations from
1,400,074 to 1,200,073 and collections from 341 to 292. The run peaked at 4,168
occupied cells.

## Chosen design

Build a precise, stop-the-world, single-threaded mark/compact collector for each
Session. Use typed arenas, repair relocatable object indices in all surviving
roots and heap edges, and compact survivors. This Nova-inspired design replaces
the earlier semispace/per-object-table proposal. Stable handles identify
registered root slots, not every object. Native code publishes roots explicitly;
there is no machine-stack or register scanning.

The first milestone reclaims existing unreachable cells and array storage under
repeated calls and long REPL sessions. Escaping closures follow after the same
rooting contract passes native tests. Generations, parallel collection, and
concurrent work are later measured changes. Mimalloc remains the allocator for
host storage and arena backing allocations.

### Decision ledger

| Decision | Resolution |
| --- | --- |
| Q1: First milestone | Reclaim current runtime garbage before adding escaping closures. |
| Q2: Collection | Stop the Session mutator at declared safepoints; collect synchronously on that thread. |
| Q3: Ownership | One mutator and private heap per Session; no cross-Session managed objects. Compiler workers access immutable metadata. |
| Q4: Budget | Include arena capacity, owned managed buffers, native mirrors, roots, and collector metadata/reserve. This is not an RSS limit. |
| Q5: Host addresses | Scoped rooted handles and non-collecting borrows; borrowed addresses cannot survive managed allocation, backing-storage growth, collection, or VM reentry. |
| Q6: Movement | Move and compact from the first collector release. |
| Q7: Native roots | Explicit registered root handles; no machine-stack/register scanning. |
| Q8: Retention | Trace live temporaries. Keep named locals inspectable until frame discard; persistent named globals remain roots. |
| Q9: Pressure | Collect first, then permit bounded growth, then return a source-linked storage-limit error. |
| Q10: Initial graph | Strong references only; no user finalizers, weak references, resurrection, or general pinning. |
| Q11: Native root lifetime | Invocation root scopes belong to resumable VM frames; helper scopes are shorter lived. |
| Q12: Rust API | Collecting/non-collecting capabilities and explicitly rooted values; lifetime-erasing escape hatches stay internal. |
| Q13: Representation | Relocatable typed object indices and stable registered root slots; no stable per-object lookup table. |

See [ADRs](adr/) and the [runtime glossary](../GLOSSARY.md).

## Current runtime and migration

`Session` now owns a chunked cell arena. With GC disabled it recycles returned
routine-local cells, while other unreachable cells and arena capacity remain
reserved until restart/drop; with GC enabled unreachable cells are compacted at
collecting boundaries. Dense and sparse arrays contain relocatable cell indices. BYREF
shares a cell; BYVAL copies array values. Strings and snapshots still use
Rust-owned `Arc`, `Vec`, and `BTreeMap` storage. The default cell limit is one
million occupied cells; named globals and inspectable locals stay rooted.

Native storage mirrors scalar values. Helpers materialize dirty values and
refresh `NativeView` pointers. Complex native `HANDLE` slots currently carry no
managed identity. OSR entries stay selective, while on-demand all-PC liveness
supports GC at collecting operations and paused continuations. Separate GC
reference kinds identify addresses independently of their pointee types.

The rest of this document specifies the complete rollout contract. Preserve value-copy
semantics, sparse holes, initialization checks, BYREF aliases, exact instruction
budgets, diagnostics, and compiled continuation behavior.

## Arenas and references

Start with the cell graph; introduce arenas for managed strings, array storage,
records, and environments as those representations migrate. Use opaque typed
indices with a 32-bit object index. Scalars retain their current representation;
this does not require NaN boxing or a language pointer syntax.

Arena backing storage uses fixed-size chunks with a power-of-two slot count,
targeting at most 64 KiB per chunk. Compact survivors toward the logical start
and release empty trailing chunks. This bounds growth increments and permits
release without reallocating an entire arena. Chunking is an allocation layout,
not selective page evacuation.

An ordinary reference identifies an arena and current object index. Compaction
may change that index. Rewrite every surviving root, cell/array entry, object
field, environment, and suspended transfer value. Logical identity and aliases
survive movement; reference bits are not public IDs. Tables keyed by managed
references must support relocation/rehashing. Future stable identity hashes must
not use movable arena positions.

A root handle identifies a stable registered slot containing a managed reference.
The collector updates that slot. Root-slot reuse has generation checks; safe host
APIs do not expose forgeable IDs. The root registry intentionally traces active
registered roots, not every arena entry or unregistered temporary.

Each managed type supplies tracing, reference-rewrite, and owned-storage
accounting operations. Built-ins use typed visitors. Future composite descriptors
cover reference-bearing inline fields and active variant payloads; infinitely
sized inline recursive layouts must be rejected, while indirect cycles are valid.

Do not raw-copy Rust owners into a byte heap. Compaction moves Rust values with
correct ownership and drops unreachable owners exactly once. During migration,
objects may own Rust containers/string buffers: charge their backing allocations
to managed storage and release them with their owners. Moving an arena entry does
not imply moving every external buffer or returning all capacity to the OS.
Packed GC-native payloads can later use the same descriptor contract.

### Collection phases

1. Materialize native writes and publish required explicit roots.
2. Mark reachable objects through registered roots and typed heap edges.
3. Compute old-index-to-new-index relocation maps for each arena.
4. Rewrite roots and reference-bearing survivor fields using those maps.
5. Move survivors with ownership-preserving operations and release dead owners.
6. Release unused chunks, rebuild native mirrors/address caches, and resume.

No mutator or host lookup observes the intermediate rewritten-but-not-yet-moved
state. No managed allocation, fallible metadata growth, VM reentry, or user code
runs during collection. Mark bits, relocation maps, and worklists have capacity
reserved before collection starts, charged to the total budget.

BYREF aliases remain references to the same relocated cell. Array-element aliases
retain logical cell identity through growth; they never point into container
nodes. Interior references retain a traced base plus validated field/offset data
and resolve a fresh address after collecting operations. Assignment preserves
value-copy semantics; representation sharing requires immutability or separately
verified copy-on-write behavior.

## Explicit roots and Rust helpers

Distinguish ordinary managed references, rooted handles, and temporary borrows.
`GcScope` authorizes potentially collecting/reentrant operations; `NoGcScope`
borrows prevent those operations while derived borrows remain usable. All managed
allocation requires the collecting capability in the first implementation.

Allocation returns a rooted result or transfers directly into a rooted
destination. Reserve root slots before exposing new unrooted objects. Root
publication uses pre-reserved storage and cannot collect; root-space exhaustion
must be handled before publication/allocation or reported as a storage-limit
failure. Root removal never runs managed user code.

Reading a root returns a reference bound to the current non-collecting capability,
not an unrestricted `'static` value. Safe helpers cannot erase that lifetime.
After a collecting call, read needed references again from roots: rooting the
same target elsewhere does not repair an unregistered copy.

Helper inputs, copies, construction intermediates, and results remain rooted
across nested allocation. New objects start with valid empty reference fields.
Partially initialized objects remain traceable. Heap objects contain ordinary
managed references, not host root handles that would independently root targets.
Internal buffer cleanup cannot reenter the VM or allocate managed data.

Rust-owned argument vectors and temporary array snapshots can also contain
managed references. Store these values/builders in owned, explicitly registered
root storage with tracing and rewrite support, or represent them as rooted heap
objects. Rooting only their individual element copies is insufficient when an
unregistered original buffer would retain stale indices after collection.

## Invocation root scopes and inspection

Invocation roots belong to VM frames, with stable backing while registered.
Budget exits, INPUT suspension, native-stack unwinding, and tier changes retain
required roots. Helpers use nested temporary scopes. Discarding an owner releases
its scope; failed frames retained for inspection stay rooted until reset/discard.

Named locals remain roots until frame discard in this release. Temporary virtual
registers use safepoint liveness and stop rooting after last use. Clear/poison dead
managed temporary slots at collection boundaries instead of retaining stale
indices. Reserve frame root capacity once; update the active temporary-root mask
at collecting boundaries.

Caller values live across a call stay published while the callee runs. Transfer
return values into caller roots before releasing callee roots. Errors unwind
helper scopes without losing inspectable frame data. REPL replacement roots old
state and staged new values until the transition succeeds.

## IR and native integration

Add instruction-level `may_collect` and `may_reenter` effects, distinct from
observable language effects and a routine-level `may_allocate` summary. A pure
`fn` may allocate and collect. Preserve failure, divergence, and fresh allocation
identity; do not merge/eliminate allocating calls solely because they are pure.

Represent scalar, managed-object, and managed-location/interior reference kinds
explicitly. A pointee's language type cannot substitute for its reference kind.
Each collecting operation describes its live temporaries, arguments, and
references needed by suspended callers.

Before a collecting helper/poll, flush dirty values and publish live references
through explicit native root handles. Root addresses remain valid during
collection. Afterward reload repaired references, refresh `NativeView`, and drop
pre-collection object-address caches. Native mirrors/cell tables are non-owning
caches that must be repaired or rebuilt; they are not implicit roots or a
permanent object-identity table.

The collector does not discover roots through the machine stack, saved registers,
or JIT stack maps. Compiler metadata drives explicit publication. Rust lifetimes
do not validate emitted machine code: bytecode/native IR verification and stress
tests must independently enforce complete publication and reloads.

GC roots and OSR recovery maps share type/liveness inputs but have separate
contracts. Cover every collecting operation and suspended continuation, including
PCs that are not initial OSR entries. An invocation already in speed code resumes
in speed through its compiled continuation. Collection must not force
interpretation, restart an invocation, repeat effects, or lose precise budget/error
PCs. Numeric native loops pay no per-instruction rooting cost; publication occurs
only at declared collecting boundaries.

## Allocation pressure and accounting

Use a configurable **64 MiB total managed-memory limit** as the initial
implementation default, allocating chunks lazily. Validate this starting policy
on allocation benchmarks before default rollout. It is neither a process-RSS
limit nor a promise of 64 MiB usable payload. Keep existing cell/frame limits
during migration; reclaimed cells no longer count as cumulative allocation.

Charge actual arena capacity, owned managed buffers, native mirrors/address
caches, root storage, and collector scratch/reserve. Charge shared backing buffers
once. Program/source metadata, executable code, and console buffers have separate
host accounting. Different engines may therefore have different usable payload
capacity under the same total limit.

Collect before growing. Release unused chunks/dead buffers, then grow
geometrically in bounded chunk increments only if projected capacity and required
collection reserve fit the limit. If live data plus required storage/reserve
cannot fit, report the normal source-linked storage-limit error. Mandatory
metadata capacity must be reserved before allocation or collection proceeds.

Recoverable budget failures occur before relocation begins; collection has no
allocation-failure path that leaves a partially repaired heap. A failed
multi-allocation operation can preserve prior language-visible writes, matching
current behavior, but must release its temporary roots. Host allocator exhaustion
is distinct and must not be advertised as recoverable without implementation
support.

Initially support strong references only: no weak references, user finalizers,
resurrection, general pinning, or cross-Session objects. Large host-backed payloads
can remain external under the owned-buffer protocol; this is not a public pinning
capability.

## Closures, composites, modules, and code

Support traced environments/captured cells, reference-bearing composites, and
module globals through descriptors and explicit root owners. Syntax and type
rules are separate work. Test cyclic heap fixtures before language syntax exists.

Non-capturing functions can use stable code identities. Capturing values trace
environments and retain required program/code ownership. Native code and immutable
descriptors stay outside the moving heap. Compiler workers receive metadata and
constant identities, never live Session indices or object addresses. Materialized
managed constants and module state have explicit Session root owners; ownership
must not keep every retired program/artifact alive forever.

Before releasing closures/modules, test code retirement, old REPL revisions,
managed constants, and cycles involving code owners. Keep allocating-call
optimizations conservative until language identity/capture contracts exist. GC
must not silently change copy, capture, or module-initialization semantics.

## Delivery and gates

1. **Binding and metadata:** add typed reference kinds, root scopes, capabilities,
   tracing/relocation, safepoint effects, accounting, and IR verification on the
   real cell/array ownership path.
2. **Interpreter collection:** compact cells, repair roots/edges, reclaim owned
   buffers, preserve aliases/copies/inspection, and stress tiny heaps with
   collection inside multi-allocation helpers.
3. **Native parity:** implement explicit publication, suspended-caller roots,
   mirror rebuilding, and same-tier continuation in Winch and both Cranelift
   tiers. Require native ARM64/x64 runtime validation. Engines that cannot honor
   GC must explicitly reject GC-enabled execution, never omit roots or downgrade
   during collection.
4. **Default rollout:** require bounded-memory repeated calls/REPL, movement and
   alias tests, exact budgets/errors, and cold/warm allocation benchmarks with
   observation disabled. Validate the 64 MiB limit and publication overhead; ship
   automatic GC only after supported engines pass.
5. **Later policy:** evaluate generations/remembered sets from measured lifetimes.
   Mature-space compaction stays stop-the-world. Only then consider parallel GC,
   optional concurrent marking, and separately concurrent relocation. GC and
   compiler scheduling remain distinct.

Route every managed reference store, initialization, and bulk copy through a
common IR/runtime contract. The initial full-heap STW collector needs no
generational/concurrent write barrier; later modes must cover all those writes.
Do not add speculative concurrent-relocation barriers to initial native loads.
Collector-specific compiled protocols require compatible code before execution.

## Validation and reporting

- Reclaim unreachable cycles and preserve reachable cycles, identity, and aliases;
  exercise fragmentation and low/high survival.
- Force movement during nested allocations, initialization, deep copies, sparse
  growth, and return-value transfer with correctly rooted intermediates.
- Preserve caller/array-element aliases through growth and collection; test stale
  root IDs, invalid interior references, and exactly-once owned-buffer release.
- Collect across INPUT suspension, inspectable errors, native recursion, every
  partial budget, and native calls/tier transitions without a tier downgrade.
- Verify dead temporary graphs die, named locals remain inspectable, frame discard
  releases roots, and REPL replacement/reset releases obsolete runtime objects.
- Test descriptor/accounting support for reference-bearing inline values and
  variable external buffers before enabling those representations.
- Test near-limit collection including root/scratch/mirror capacity, no partial
  relocation on failure, and predictable errors when required live storage cannot
  fit. Never trace every arena entry as a root.
- Add Rust compile-fail tests for borrows crossing collecting calls and root-scope
  escape, plus positive rooted cases. Use debug epochs/poisoning and applicable
  memory tools alongside native execution tests.
- Benchmark no-allocation scalar loops, short-lived allocations, long-lived/high-
  survival graphs, BYVAL copies, repeated calls, and long REPL sessions. Separate
  collector overhead from observation overhead.

Report cumulative allocation, live/occupied bytes, capacity, external managed
buffers, roots, native caches, scratch/reserve, bytes reclaimed/moved, collection
count, and phase/pause duration. Distinguish reusable capacity from RSS returned
to the OS. GC pauses are separate from VM execution/compiler work and included in
run wall time. Detailed timing/history is opt-in and bounded; disabled reporting
adds no per-instruction clocks or event allocations.

## Reference designs

- [Nova GC guide](https://github.com/trynova/nova/blob/591bf7ba0d35b095656dd31cdf397ef813a17fc2/GARBAGE_COLLECTOR.md)
  and [collector](https://github.com/trynova/nova/blob/591bf7ba0d35b095656dd31cdf397ef813a17fc2/nova_vm/src/heap/heap_gc.rs):
  scoped roots, capabilities, typed compaction and index repair. Root-slot indices
  differ from object indices; binding is not rooting. Our chunking, native
  protocol, and budget policy are local choices, not claims about Nova.
- [V8 indirect handles](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/docs/heap/handles.md),
  [GC overview](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/docs/heap/garbage-collection.md),
  and [compaction](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/docs/heap/compaction.md):
  scope escape and later generational/selective-evacuation policy references.
  Exclude the [enabled direct-handle scanning path](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/src/handles/handles.h)
  under Q7. Branch sources were checked on 2026-11-10; pin relied-on revisions if
  implementation adapts source.
- [V8 Orinoco explanation (2019)](https://v8.dev/blog/trash-talk): historical
  context for reserve, generations, and concurrency terminology.
- [MMTk binding guide](https://docs.mmtk.io/portingguide/howto/nogc.html): future
  VM-binding/toolkit comparison if collector complexity warrants it.

These projects are references, not dependencies or performance evidence for this
runtime. Source reuse requires its own provenance/license review.
