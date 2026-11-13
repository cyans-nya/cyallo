# Language integration progress

This file tracks implementation work. It does not declare the full concurrency
design complete. The runtime foundations are described in [concurrency.md](concurrency.md).
Implementation requirements live in [compiler and runtime rules](compiler-and-runtime-rules.md),
which are mandatory through the root AGENTS.md.

## Implemented in the current working tree

- `gen proc` and `gen fn` closure syntax, with an explicit factory result
  `Generator<Yield, Resume, Return>` and ordinary construction arguments.
- Pure `gen fn` rejects captures. `gen proc` captures using closure conversion.
- Lexical `yield`, resume input as an ordinary typed value (including Result),
  Yielded/Complete steps, exclusive resume, idempotent close, owned `yield from`.
- Stackless VM activation fields, explicit GC roots while suspended, owned
  delegate and RC cleanup, and factory laziness.
- `.ya` examples in `examples/concurrency/generators/` and `tests/generators.rs`
  exercise eager/deferred checking, all native engines, moving GC stress, and
  single-instruction engine/interpreter handoffs.
- Root registries are becoming instance-owned Arc/Mutex registries so external
  host roots may be retained/dropped safely as their exclusive Session migrates.
  Session is not Sync. Narrow Send implementations cover stable private slot
  backing, raw registered root descriptors, owned backing allocations, and native
  service table pointer storage; no blanket unsafe Send implementation for Session.

## Required next work

1. Finish migration regression tests and check the existing GC/native suites.
   Generator identity allocation must bound live instances instead of cumulative
   creations. Resource-owning factory captures and implicit close need coverage.
2. Language values need a separate transport/resource module: detached owned
   payloads, explicit logical resource handles, task joins, sender/receiver
   ownership, clocks, and groups. Raw Session cell indices must never cross Cs.
   Cloned Rust Value storage must not duplicate logical endpoint ownership.
3. Connect source standard declarations and typed lookup operations to the C/T/P
   scheduler: spawn, joins, cancellation/groups, MPMC channels, selection, clocks.
   Ordinary procs park without async/await coloring. Store pending operation,
   successor PC, and result destination before beginning its side effect.
4. Worker Cs own independent movable Sessions. Root host execution must lease a
   P from the same runtime, including native entries. Add explicit scheduler
   execution permits with instance-owned T IDs, and hand off Ps to waiting host
   execution between worker quanta; use no scheduler TLS or global counters.
   Worker Context already owns its P and supplies its waker/fuel explicitly.
5. Add Waiting-for-runtime status and wake handling in Session, native drivers,
   CLI/debugger/REPL. A pending operation must not replay its bytecode or charge
   busy-wait instructions. A host root waits outside its P; worker Cs return
   Pending to the scheduler. Share output through runtime-owned storage.
6. Enforce transport recursively over arguments, captures, results, saved state,
   erased values, and transitive global dependencies. Default-local managed
   graphs and references must not cross Cs. No implicit local-to-shared promotion.
7. Implement explicit shared/unique qualification separately from GC/RC storage,
   whole-graph uniqueness including weak/outside aliases and borrows, ordinary
   user-defined `share` conversion, guarded shared mutation, synchronized shared
   RC, and coordinated tracing of shared roots. Preserve module/capability limits.
8. `examples/concurrency` contains the working Cyallo generator set. The
   mistakenly added Rust example programs and their package have been removed.
   Add language scheduling/channel/ownership examples and real CLI regressions
   as their integrations become executable. Do not add Rust example programs or
   external Rust integration work.
9. Runtime and backend failures are typed: `Status::Failed` carries a
   `RuntimeFailure` (`RuntimeError` plus span), native faults and exit reasons
   are decoded enums, and backends return `CompileError`/`EngineError`. Remaining:
   task/channel failures must reuse these types when language integration lands,
   and compile-time diagnostics still carry free-form messages. Resolve
   primitive/member identities before lowering and specialize using types
   rather than source spellings.

The agreed native model remains heap VM continuations with all-PC fallback;
native machine-stack switching is deferred. Root return stops unfinished Cs;
explicit groups/wait/cancel-and-join provide graceful cleanup. Result::Err is a
normal returned value; runtime failures cancel group siblings and retain diagnostics.
