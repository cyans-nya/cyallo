# Pending decisions and unfinished work

Recorded 2026-11-17 after the data-enum / core-enum work (commit `d6ebc04` on `main`).
Each file lists what was **decided but not implemented** or is **still open**, with the
current state, what blocks it, and the question that needs an answer. Nothing here is
work assigned to the user unless a file says "Decision needed".

| File | Topic | Kind |
| --- | --- | --- |
| [01-core-module.md](01-core-module.md) | Always-supplied `core` module | Decided, not implemented |
| [02-storagekind-removal.md](02-storagekind-removal.md) | Deleting `StorageKind`, role tags, old intrinsics | Decided, partly implemented |
| [03-layout-optimizer.md](03-layout-optimizer.md) | Shape-based enum layout, no role special-casing | Decided, not implemented |
| [04-source-bodied-methods.md](04-source-bodied-methods.md) | Cyallo bodies for enum/struct methods | Decided, not implemented |
| [05-sdk-worlds.md](05-sdk-worlds.md) | SDK/world phases not built | Decided, partly implemented |
| [06-memory-lifetimes.md](06-memory-lifetimes.md) | Scope-based cell reuse without GC | Decided in principle, partly mitigated |
| [07-baseline-failures.md](07-baseline-failures.md) | 15 failing tests carried since the baseline | Open |
| [08-performance-and-checks.md](08-performance-and-checks.md) | Slowdown, benches, Clippy, format | Open |
| [09-branches-and-repository.md](09-branches-and-repository.md) | `main` vs `overhaul`, pushing, stray files | Decision needed |

Superseded: [../pending-work.md](../pending-work.md) is the earlier agent's handoff. It
describes `TaskHandle`/`JoinResult` renames and `StorageKind` cases that have since
changed; use this folder instead. Execution history: [../data-enums-work-log.md](../data-enums-work-log.md);
detail on the marker removal: [../storagekind-removal.md](../storagekind-removal.md).

## What is implemented (for reference)

- Opaque types and provider bindings for Clock, Task, Cell, Vec/VecDeque, channels, weak
  references, allocation and layout; contract tables for intrinsics and members.
- `Option`, `Result`, `GeneratorStep`, `SendError`, `ReceiveError` values are general enum
  values with reserved variant ids; annotations and native signatures produce the declared
  enums; `std::option::some/none` and `std::result::ok/err` are removed.
- Persistent compile worker pool (process-wide by decision), structural module-cache keys.
- Per-match payload cells and overwritten enum payload cells are reused without GC.

## Backend decisions

- The process-wide compile pool supplies capacity only. The replacement must
  enforce per-instance queue and in-flight caps, take jobs round-robin across
  instances, and retain zero instance state after each job returns. The
  owning runtime validates and publishes results from its own mailbox.
  [Compile worker capacity](../backend-design.md#compile-worker-capacity)
- Code quality, auditable structure, and correctness govern acceptance.
  Benchmark results document the measured costs and guide changes.
  [Code quality rules](../backend-design.md#code-quality-rules)
- Use the SDK registration definitions of SDK, provider, binding, installed
  table, and bound call throughout the replacement. The first shared
  interpreter and SDK fixture executes a bound call through the installed
  table. [SDK terminology](../sdk-registration.md#terminology)
