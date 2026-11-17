# Language integration progress

This tracker records verified language behavior. The runtime contract lives in
[concurrency.md](concurrency.md); required implementation practices live in
[compiler and runtime rules](compiler-and-runtime-rules.md).

## Implemented and exercised in Cyallo

- Stackless `gen proc` and capture-free `gen fn`, lazy factories, lexical `yield`,
  owned `yield from`, typed resume values (including Result), completion, close,
  suspended GC roots, and RC cleanup. `start()` begins a generator; subsequent
  `resume(value)` supplies its input. Unit-input `next()` starts or continues.
  Startup/order misuse has typed runtime failures.
- Task spawn, consuming join, cancellation, completion/failure/cancelled/stopped
  outcomes, and detached task handles. Returned `Result::Err` is ordinary data.
  Worker runtime failures preserve typed failure causes.
- Bounded and unbounded MPMC channels, zero-capacity rendezvous, competing
  receivers, endpoint clone/close, send/receive and nonblocking variants. Typed
  send errors retain unsent values; receive errors distinguish empty and closed.
- Explicit clock capabilities and millisecond sleeps from ordinary procedures.
  Source procedures require no `async`/`await` coloring.
- Independent worker Sessions with detached owned payload transport. Primitive,
  array, record, enum, option/result, and supported capability values cross Sessions
  without transmitting heap cell indices. Logical endpoint ownership is distinct
  from cloned internal Value storage.
- Local managed objects and borrows are rejected as channel elements, task
  captures, and task results. An accepting function cannot turn an unproven
  callable parameter into spawn authority. Worker global access is guarded by a
  typed runtime failure; static transitive global analysis remains pending.
- Storage uses `gc`/`rc` with independent locality. `@local` explicitly spells
  the default in types and constructions, including custom allocator/lifetime
  forms. `@shared` is retained in AST/types but rejected during compilation
  while shared managed graph support is pending. Legacy storage sigils are
  rejected with a migration diagnostic. `tests/managed_syntax.rs` covers these
  boundaries and preserves locality through generic checking.
- Shared C/T/P scheduling budget for root and worker execution, including native
  root entry. Waiting releases the P. Pending operations retain their destination
  and successor PC and resume without replaying effects or charging busy waits.
- Waiting/wake integration in VM, native drivers, CLI, debugger, and REPL. Root
  completion stops unfinished tasks; REPL entry completion preserves its runtime
  and capabilities for later entries.
- Cancellation withdraws pending operations and unwinds owned resources and
  generators; user finalizers may park during cleanup. Abrupt stop releases
  logical resources without executing guest cleanup.
- Instance-owned runtime state and synchronized root registries. Sessions remain
  exclusively accessed and are movable between threads; there is no scheduler
  TLS, global runtime service, or process-wide identity counter.
- Runtime `.ya` loads through `std::module::load(path)` suspend through the
  bounded host pool, inherit scoped standard-module permissions, and publish
  append-only code at a VM safe point. Checked module products retain their own
  routines and initializers through linking and publication; see
  [module compilation](modules.md). Readonly namespaces use general object
  properties/calls and retain identity through optional structural views.
  Imported procedures use the caller's local heap. Independent one-P coroutine
  loads and suspended generators are covered in `tests/dynamic_import.rs`;
  examples are in `examples/concurrency/modules/`. Native drivers fall back to
  the interpreter after publication; see [runtime loading](dynamic-import.md).
- Real standard `.ya` declarations, private annotated ABI bindings, ordinary
  module/import resolution, source-driven generic/member contracts, and scoped
  import permissions. See [standard declarations](standard-declarations.md).

The executable examples are in [examples/concurrency](../examples/concurrency/README.md).
`tests/concurrency.rs` and `tests/generators.rs` cover eager/deferred checking,
interpreter/Winch/Cranelift engines, moving GC stress, single-instruction handoffs,
and one-P progress. CLI tests execute the language examples; REPL tests cover
capability and native-alias persistence. Runtime crate tests additionally verify
the scheduler, channel commit/withdrawal protocol, timers, and blocking bridge.

## Remaining work

1. Source interfaces for groups, select/offers, and coroutine input capabilities.
   Runtime foundation APIs alone do not establish source-language support.
2. Execution of `@shared` managed storage and unique-to-shared conversion,
   whole-graph uniqueness including weak/outside aliases and borrows, guarded
   shared mutation, synchronized shared RC, and coordinated shared tracing.
3. Static transitive global/capture proofs, including erased values and saved
   state. Retain the worker global-access runtime guard until proven unnecessary.
4. Native execution of child coroutine continuations. Child procedures currently
   run in the interpreter; the root may run in any supported native engine.
5. Relinking suspended generators across REPL program snapshots. Ordinary
   function/native aliases and clock capabilities have regression coverage;
   suspended generator persistence is not claimed.
6. Broader ownership/finalizer rejection and shutdown coverage as those remaining
   source capabilities are introduced. Keep examples in Cyallo and distinguish
   runtime-only support from verified language behavior.

The agreed native model remains heap VM continuations with all-PC fallback.
Native machine-stack switching is deferred.
