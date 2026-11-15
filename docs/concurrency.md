# Concurrency runtime

Concurrency implementation follows the [compiler and runtime rules](compiler-and-runtime-rules.md).

Cyallo's runtime separates C/T/P scheduling, MPMC channels, owned host operations,
timers, and a bounded blocking bridge into focused crates. These are runtime
implementation modules. The language interface is resolved and type-checked by
the compiler before VM execution.

The [Cyallo language examples](../examples/concurrency/README.md) exercise the
generator, task, channel, timer, and owned payload integration. Shared managed
graphs and several source capabilities remain pending; see the
[implementation tracker](concurrency-language-progress.md).

| Crate | Owns | Dependencies |
| --- | --- | --- |
| `cyallo-scheduler` | Coroutine lifecycle, P leases, T workers, run queues, joining, groups, cancellation | `cyallo-io`, `cyallo-clock`, `cyallo-host` |
| `cyallo-channel` | MPMC queues, endpoint lifecycle, rendezvous, selection, buffer quotas | Standard library |
| `cyallo-io` | Single-commit operations, completion and cancellation | Standard library |
| `cyallo-clock` | Timer registration and an explicit clock capability | Standard library |
| `cyallo-host` | Bounded host work queue and OS workers without a P | `cyallo-io` |
| `cyallo-modules` | Source read grants, declared file loading and typed import/publication errors | `cyallo-ast`, `cyallo-parser`, `cyallo-host` |
| `cyallo-concurrent-values` | Heap-independent payloads and logical capability ownership | `cyallo-ast`, `cyallo-channel`, `cyallo-scheduler` |

There are no process-wide runtime registries, counters, default factories, timer
services, or thread-local scheduler identities. A Runtime owns its coroutine
counter, operation registry, timer service, and host pool. Each Factory owns its
channel coordination, selection rotation, buffer quota, and operation counter.
Cloning a capability shares that instance; constructing another instance creates
independent state. Numeric IDs have meaning only within their owning instance.
Completion handles carry object identity directly, so equal numeric IDs in two
registries cannot complete each other's operations.

## Execution and lifetime

A **C** owns an erased, resumable `Coroutine` and its eventual outcome. A **T** is
an OS thread. A **P** is an execution permit with its own runnable queue. A T must
lease a P before polling a C. P ownership and C runnable/running transitions are
checked independently. `Config::processors` bounds simultaneous execution;
waiting Cs do not retain a P. The default uses available host parallelism.

Workers use per-P queues, a global injector and batch work stealing. Every 64th
dispatch checks the injector before local work. A coroutine receives an explicit
Context carrying its identity, P preference, waker, and instruction fuel. VM
continuations must stop at that fuel boundary. Internal polling adapters are
implementation details. Cyallo procedures suspend normally, without an
async/await distinction in the source language.

Dropping a Task detaches its join handle. It does not cancel the C. Joining is
single-consumer and parks the calling C. Internal blocking control paths must
not run on a scheduler T. Child spawners carry P preference and group membership
explicitly; there is no ambient thread-local executor lookup.

`Outcome::Completed(Err(...))` is ordinary successful execution. `Context::fail`
reports a runtime trap; Rust panics are also contained at the coroutine boundary.
Either path cancels group siblings immediately, then polls the failing C's
cleanup. Cleanup can park. Independent Cs continue. The runtime records failures
even when their Task was detached.

Groups seal external submission when waiting. Live admitted group members may
still spawn children during the drain. `wait_async` and `cancel_and_wait_async`
park the caller instead of occupying a T. Host-side `wait` and `cancel_and_wait`
block. Runtime `cancel_all` permanently closes submission, requests cleanup for
all admitted Cs, and wakes parked Cs; `cancel_and_wait` also drains them.

`stop` is abrupt: unfinished Cs become Stopped and their owned Rust state is
released, without running their coroutine cleanup protocol. Dropping the final
Runtime stops and joins execution workers. Host callbacks already running on
blocking workers retain their owned arguments until they return; shutdown does
not wait indefinitely for an uninterruptible OS operation.

## Operations, clocks, and blocking work

Register an operation, its result destination, and its continuation PC before
beginning the host effect. An early completion is retained even if the C has
not parked yet. Resume the saved continuation rather than repeating the effect.

Completion and cancellation compete for one commit. Duplicate or cancelled
completions return their payload to the adapter. Cancellation cannot undo a
committed completion. The last completion handle disappearing cancels an
uncompleted operation and wakes its waiter. Cancellation hooks, callback
captures, and wakers are released outside operation locks. Adapters must own
their arguments and buffers across Pending; they must not retain raw VM pointers
or borrowed guest storage.

`runtime.clock()` grants an explicit Clock. Dropping a Sleep unregisters it.
Stopping or dropping its TimerService releases pending sleepers; a Clock does
not keep its service or runtime alive. Blocking host jobs use a separate bounded
thread pool and queue. `blocking_threads` and `max_blocking_jobs` configure those
limits. Queue rejection returns an ordinary operation error. Stopping the pool
cancels queued jobs. A running callback can complete after its caller cancels;
that completion is rejected safely.

## Channels and selection

Channel coordination and quotas belong to an explicitly owned Factory within the
runtime. Language declarations resolve to typed channel operations and retain
their element and capability information through lowering.

Sender and Receiver are clonable; receivers compete for messages. Capacity zero
is rendezvous. Bounded channels preserve FIFO enqueue order. Explicitly unbounded
channels still obey their Factory's total buffered-message quota. Quota and
closed-channel send errors return the owned payload. Both endpoints can close
idempotently; buffered values drain before Closed. The last sender closes the
channel. The last receiver closes it and discards queued values outside locks.

An Offer is inert until polled or selected. `select(Vec<Offer<T>>)` requires all
arms to come from the same Factory. It commits exactly one arm, including paired
rendezvous between two selections. A selection cannot rendezvous with itself.
Losing receives consume no value; losing sends return their payloads in
`Selected::unsent`. Default and timeout cases withdraw all offers atomically.
Timeouts receive an explicit clock future. Readiness scans rotate their starting
arm within the Factory. Rust selections currently have one payload type T.

The current implementation serializes channel transitions within a Factory to
make paired commits and withdrawal atomic. Separate factories operate independently.
A future performance change can add per-channel fast paths while preserving the
commit protocol; this implementation makes no lock-free or throughput claim.

## Compiler and VM integration still required

Source generators now lower to stackless VM state machines and preserve moving
GC roots across suspension. Session ownership can migrate between execution
threads, with synchronized root registries and private stable slot storage;
Sessions remain exclusively accessed. Source task/channel operations transport
detached owned payloads. Shared managed transport remains in progress.
Heap-relative Values must not be sent between Sessions.

The remaining integration has these required boundaries:

1. Coroutine-owned heap VM continuations with instruction-budget suspension and
   task-local native views. Publish immutable code entries and reload VM pointers
   after helpers or collection. Native machine-stack switching stays deferred.
2. Local-by-default managed graphs confined to their C. No implicit promotion to
   shared at function or spawn boundaries. Explicit sharing requires uniqueness
   of the whole reachable graph, including outside aliases to children, weak
   references, and borrows. Shared mutable access goes through guarded Cell/Mutex
   capabilities. The checks must survive captures and type erasure.
3. Coordinated tracing and roots for parked Cs, pending operations, channels,
   results, and generators. Shared RC uses synchronized ownership operations.
4. Stackless `gen proc` and `gen fn` factories, lexical yield, typed resume input,
   Yielded/Complete results, exclusive resume, close, and owned `yield from`.
   Generator yields return to the caller; an I/O wait parks the whole C. Guards
   may survive either suspension; borrowed yield values are rejected.
5. Resumable I/O capability calls retain operation identity, result destination,
   and successor PC before submission, and never repeat host side effects.
   Module visibility and passed capabilities must survive spawn/resume/load
   without granting new ambient authority.

## References

The C/T/P roles follow the [Go runtime scheduler model](https://go.dev/src/runtime/HACKING).
[Go channels](https://go.dev/ref/spec#Channel_types),
[Kanal](https://github.com/fereidani/kanal), and
[Crossbeam channels](https://docs.rs/crossbeam-channel/latest/crossbeam_channel/)
are the channel design references. The coroutine-facing implementation owns its
commit and cancellation protocol rather than adapting a blocking receive call.
