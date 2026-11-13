# Architecture

Implementation changes follow the [compiler and runtime rules](compiler-and-runtime-rules.md):
resolved declaration identities and type-directed specialization, typed enums
and errors, and explicitly owned runtime state.

```text
borrowed UTF-8 source → compact tokens → source-preserving AST
→ declarations/signatures → per-body fixed-type inference + semantic checks
→ typed control-flow IR → typed register bytecode
→ interpreter | native Winch | Cranelift fast | Cranelift speed
```

Independent file/declaration parsing, semantic function units, per-function metadata,
and bytecode translation use bounded parallel jobs. Shared signatures, generic
instances, and effects have ordered coordination barriers. See
[parallel compilation](parallel-compilation.md) and [general lookup plans](lookups.md).

Modern mode is the default. Compatibility preserves CAIE syntax and imperative
functions. `NativeBackend`/`Executable` provide the extension point for a future
LLVM backend.

The [concurrency runtime](concurrency.md) separates C/T/P scheduling, channels,
single-commit operations, clocks, and blocking host work into independent crates.
Those APIs currently support owned host continuations; source-language and VM
heap integration are described there as remaining work.

## Declarations and deferred analysis

`compile` and `compile_with_options` fully validate every body. The CLI uses
`compile_for_execution` for automatic JIT and interpreter runs: declarations,
parameter types, duplicate names, signatures, and the entry body are eager;
routine inference, body checks, effects, IR, liveness, and bytecode are deferred
until use. `check`, the bytecode command, explicit eager engines, debugger, and REPL retain
full validation. Parsing still checks the syntax of every body.

An immutable declaration exposes names, signatures, spans, and a cheap syntax
work estimate. Its body is initialized once through a shared `OnceLock`; sessions
and compiler workers reuse the same result, including cached diagnostics. A
called body must pass its checks before frame allocation/body effects. Deferred
errors retain the original diagnostic code, source span, and related labels.
Unused bodies need no detailed analysis. Explicit body inspection can initialize
one, and `Program::validate_all` forces all bodies.
Compile-time constant initializers and their reachable static callees are checked
eagerly, including when ordinary execution uses deferred routine analysis.

Modern `fn` permits private/local mutation, owned value-copy reads and constant
reads, but rejects mutable managed reads, ref captures, I/O, mutable globals,
nondeterminism, and procedure calls. State-reading lookup mappers use procedures
with separate readonly checks. Effectful `proc` can return
`-> T`. Eager validation computes transitive effects. Deferred bodies check local
purity against function declaration contracts; every callee is checked before
entry. Cold-call resource effects are conservative and marked
`transitive_complete=false`. Purity never erases possible failures, allocation,
nontermination, or instruction-budget consumption.

Modern constants are checked and evaluated before final lowering, using the same
interpreter with separate limits: 100,000 instructions, 64 frames, 16,384 cells
and 1 MiB of charged value/string allocations. Statically reachable callees are
validated even on untaken branches. Scalars are substituted as literals; strings,
records, tuples and fixed arrays can share address-free immutable pool payloads.
Loading a pool value produces an owned snapshot; mutable storage is materialized
through the ordinary rooted copy helpers. Service keys retain declaration identity.
Captured callable constants and constants declared inside closures are not yet
supported. Runtime `let` is immutable; `let mut` permits mutation.
Named constant routine references retain declaration identity under local name
shadowing and can lower to direct calls; mutable routine bindings remain dynamic.

## Source positions and bytecode

Spans use `oxc_span::Span`: half-open u32 UTF-8 byte offsets. Syntax nodes implement
GetSpan/GetSpanMut, and parentheses retain their node. SourceFile indexes lines
separately. Lexer text is borrowed by span; token buffers use Box<[Token]> and AST
text uses Box<str>. TokenKind is one byte; tokens are 16 bytes. Operators use a shared typed enum; runtime strings and compiled constants use Arc<str>.

Bytecode uses explicit opcodes, scalar tags, register operands, indexed branch
PCs, a separate source map, and pooled variable-sized operations. Words are 16
bytes. Original typed IR remains available for inspection and metadata. Bytecode
for deferred bodies is lowered once on first access, sharing the checked body.

`bytecode` disassembles this form. `--wasm N` displays the legacy Wasm adapter for
routine N. That adapter needs matching runtime imports and metadata; it is not a
stable persisted package or a standalone WASI executable. The default baseline
consumes bytecode directly. `wasm-adapter` enables the old Wasmtime runtime path
for differential benchmarking only.

## Storage and native code

VM frames retain the function ID, bytecode PC, registers, local cell IDs, and
return destination. Scalar locals and BYVAL parameters whose addresses stay
within the invocation live in frame registers. Direct frame accesses preserve
instruction PCs and initialization checks. Locals passed by reference and arrays
use cells. Opt-in GC relocates cell indices and repairs registered roots and
surviving edges together. BYREF shares cell identity, and BYVAL snapshots arrays
and copies them into private callee storage. Sparse arrays allocate populated
cells and track logical length independently. Routine entry borrows checked metadata.

Native Winch and Cranelift share `(Session*, NativeView*, fuel) -> remaining_fuel`.
The repr(C) view exposes canonical scalar registers, cell pointers, local/global
IDs, PC, counters, entry slots, and control polls. Native slots contain bits and
initialization/type/dirty tags. Known types select scalar operations; REAL uses
binary64 without NaN boxing. Native cell chunks preserve slot addresses between
collecting helpers; GC rebuilds these non-owning mirrors after relocation.
Frame/table pointers can move at helpers and must be reloaded afterward. Scalar
storage is created lazily on first native entry, including suspended callers.

Constants, addresses, scalar loads/stores, moves, checked integer arithmetic,
REAL arithmetic, comparisons, and branches execute directly. Overflow,
uninitialized reads, zero divisors, and invalid FOR steps branch to cold errors.
Fuel checks preserve exact instruction budgets, including partial blocks and
failures. Pending counters and PCs are materialized at actual yields/helpers.
Dirty register tags and a deduplicated cell list synchronize owned Values before
Rust inspects state. Allocation, strings/arrays, builtins, I/O, and call frame
setup/return use helpers.

Cranelift plans direct inlining from already checked immutable routine bodies.
The frameless subset uses integer/boolean SSA values, checked arithmetic and
small acyclic call trees. Whole-region fuel, depth, argument tags, reporting and
callee promotion guards precede execution. Arithmetic failures commit no state
and fall back to the original call, preserving error frames and source PCs.
Per-callee work/calls and separate pending inline work preserve exact accounting;
readiness polling includes the inline work clock.

Small effectful procedures can optionally share an embedded body in the caller's
unit. The ordinary call helper creates a real frame; native code branches into
the embedded body without a separate native entry. The frame carries the source
routine/PC, roots, aliases and return continuation for each invocation. Helpers,
input, errors, budget exits and GC keep their ordinary contracts. Internal returns
resume the exact caller PC, including selective speed units. An arbitrary-PC
resume bridge may still execute a call normally. Full reporting disables inline
entry, preserving observed frames and per-tier metrics. This procedure path is
opt-in (`CraneliftConfig.procedure_inlining`): measured code growth
does not generally repay the retained frame/helper costs.

The CLI's `--jit-report` captures counter baselines and renders logical work,
calls, inline counts and compiler events after execution. It leaves Session
tracing disabled, preserving optimized execution. REPL baselines are captured
after each accepted Program replacement, so summaries cover only that entry.
`--jit-trace` explicitly enables full Session reporting, physical invocation
events and per-tier clocks. Only this intrusive mode disables inlining. Summary
execution time includes helper-driven checks, compiler admission waits and GC;
it does not claim the separate phase timings that require full tracing.

Both paths share a 256-bytecode growth budget per caller; frameless callees are
limited to 32 reachable bytecodes, 64 expanded bytecodes per site and three inline
levels. Embedded procedure bodies contain at most 32 bytecodes. Dynamic calls
stay out of line. Program identity pins semantic dependencies, including folded
constants; tier changes do not change that identity. See
[implementation and measurements](routine-optimization-results.md).

The experimental collector uses a precise stop-the-world mark/compact pass over
chunked cell storage. Fixed frame/global slots and owned helper values register
explicit roots; machine stacks/registers are never scanned. On-demand all-PC
liveness deactivates dead temporary roots while named locals remain inspectable. GC
effects and reference kinds are backend-independent metadata. A collecting
helper materializes writes before collection and reloads the view afterward;
Winch and both Cranelift tiers retain their continuations. `--gc-stress` collects
before every cell allocation, including nested copy/construction helpers. The
cell limit is enforced; the planned total byte budget is still pending. See
[the GC plan](gc-plan.md) for rollout gates.

The Winch fork exposes a small ISA assembler seam. The first-party frontend emits
native branch labels and platform C ABI frames without Wasm/VMContext translation.
A write-through cache reuses the last scalar within straight-line regions;
arbitrary-PC landing stubs restore the expected cache from canonical slots.
Addressable cells are never cached across aliases. Raw bytes are published by
Cranelift JITModule, which owns executable memory, fixups, and instruction-cache
synchronization. Both native implementations use that lifetime owner. See
[vendor maintenance](../vendor/README.md) for exact-source reconstruction.

A fixed entry table supports mixed-tier native calls. Installation happens on the
execution thread between native runs while executable owners are borrowed.
Callees may return or yield in deeper VM frames. Machine-call nesting is bounded
at 64; deeper recursion uses VM dispatch. Helpers refresh storage pointers and
restore caller engine/version metadata. Program Arc identity prevents executing
code against unrelated layouts. Code is freed only after executing borrows end.

The CLI and benchmark executable use mimalloc. The library does not select an
embedding application's global allocator.

## Compilation service and admission

A `CompilerPool` owns the compiler threads, queue and cost history. Each
`TieredBackend` creates one (or shares one through `with_compiler`), and every
artifact it compiles holds the pool alive; dropping the last handle closes the
wake channel and joins the workers. There is no process-wide compiler service.
A pool has `min(3, logical CPUs - 1)` workers, with one on single-core or
unknown hosts. Threads are named `cyallo-codegen-N`. Jobs own immutable program
metadata, function IDs, and weak artifact-owner tickets, never live sessions.
Kanal channels transport bounded, coalesced wakeups. An authoritative priority
heap retains jobs and a mailbox retains completed artifacts, so a full wakeup
channel cannot lose work or block a worker behind an idle execution thread.

Each request retains the highest desired tier, queued/running tier, generation,
priority, and request time. Upgrades replace stale heap tickets; upgrades during
a running job enqueue immediately when it finishes, without waiting for a driver
poll. Hot upgrades precede ordinary/cold work; every sixteenth selection admits
the oldest request. Dropped owners cancel queued work. Program-bound mailboxes,
generation checks, and monotonic installation prevent stale results/downgrades.
Running compilation cannot be preempted.

Automatic tiering is work-based and starts with the cheapest native tier. A
routine stays interpreted only until it reaches `winch_calls` (8 entries) or
`winch_instructions` (2,000 executed instructions), then requests Winch, whose
compilation is sub-millisecond. Cranelift fast replaces Winch once the routine
reaches `warm_instructions` (5,000 executed instructions). These thresholds are
admission heuristics; compilation is asynchronous and a short run can finish
before recovering its cost. Speed codegen is requested only
for a loop-only body that has executed at least `hot_instructions` (8,000,000)
instructions and is no larger than `speed_size_limit` (512 bytecodes); speed
compile time grows sharply with body size. Unsupported Winch compilation records
its fallback reason and uses fast code.

Native routines notify the driver at their next unrequested tier threshold,
including Winch-to-fast for loops and repeatedly called leaves. Limits use the
highest requested or installed tier, so pending/failed requests cannot repeatedly
yield at the same threshold. Speed-ineligible routines still notify for fast.
`use_winch` can disable the baseline tier.

A syntax-only estimate records loops, direct-call edges and node count without lowering a deferred body. Local callable bindings are excluded from direct edges. Predictive admission considers the next eight direct calls, through two breadth-first levels, and repeated/loop work. The default half-open window is [16, 128) estimated words and is checked again against actual bytecode on a worker. Per-tier/size-bucket histories retain sixteen clean samples and use the upper quartile to predict compile cost. Full traces and dumps do not train that history.

Each artifact permits four outstanding eager jobs, a cumulative 256 estimated speculative words and 2 ms predicted work. Explicit `always_eager` bypasses automatic size/work gates but retains the outstanding-job cap and a 4,096-word routine limit. First calls share one 1 ms wait budget for the artifact; a timeout continues interpretation. `first_call_wait = 0` disables waiting. `hint_call_soon(fi)` supplies embedding-level call evidence while retaining size/source/budget gates. Source hints scale nonzero call/work thresholds: hot halves them (minimum one), cold multiplies by four with saturation.

Independent immutable routines compile concurrently. A global worker pool reserves one logical core when possible and caps native code generation at three workers. Per-artifact atomic slots also honor `--jobs`; blocked artifacts do not prevent the pool from serving other programs. Queue ownership, same-tier priority upgrades, aging, cancellation and request coalescing stay independent of the wake channel. A final-size rejection atomically retires only its still-speculative request, so it cannot cancel a newer observed-work request. Code installation and all Session effects stay on the execution thread.

Optional inspection records at most 256 unique coalesced notices. Workers collect them; CLI execution drains them at safepoints. Typed IR and bytecode formatting operate independently on already analyzed bodies. Native dumps include Program snapshot, routine, code version, optimization and continuation identity. REPL windows share a snapshot-number allocator but have separate notice buffers. Dump files are atomically renamed into place. No inspection option implicitly checks an unrelated cold body. See [the inspection/admission contract](jit-inspection-and-admission.md).

The interpreter uses bounded quanta. Compiled code receives the remaining caller
budget and polls transitions/readiness at selected branch targets/loop headers and
64-instruction checkpoints. Unchanged control state continues natively; the old
mandatory native return every quantum is gone. Runtime helpers refresh polling
state. The configurable notification interval does not delay a tier threshold:
the next eligible native safepoint yields when that threshold is crossed. Eager
code has no compiler epoch and disables readiness polling. Exact fuel checks
remain independent.

Explicit `cranelift` and `cranelift_speed` both eagerly compile every eligible routine
(`always_lazy` routines retain interpretation)
with all-PC resume entries and link their native entry slots. They share dispatch and
resume structure; their defaults differ in `opt_level=none` versus `speed`, with
backtracking allocation in both. Eager code does not compile selective-entry
continuations or build a large combined compiler graph. `CraneliftConfig` independently selects optimization, scope,
resume strategy, and allocator for controlled comparisons. The automatic JIT
retains replaceable per-routine units and selective speed entries. Cold compile
cost is distinct from warm execution speed.

## VM-frame OSR

Interpreter, Winch, fast code, and eager speed code can resume every bytecode PC.
Automatic JIT speed units accept only function starts and loop headers, reducing
external entry edges in optimizing IR. Backward edges identify headers.
Backwards liveness computes live-in virtual
registers for those entries; recovery locations refer to canonical slots,
including address cell IDs. Shared locals/cells preserve aliases and one-time
side effects.

A speed installation retains an available lower unit. A budget resume at another
PC uses that unit (or interpretation) until the next optimized entry. Lower native
code yields there before executing the header. Native polls yield for hotness,
compiler notifications, OSR, exact budgets, and errors. Nested callers propagate
the reason without losing pending work or restarting an invocation. Code versions
identify executed artifacts; immutable program identity binds their maps.

This is VM-frame transfer. Machine-stack reconstruction, speculative guards,
assumption invalidation, and native-frame deoptimization are not implemented.
Canonical values are materialized before handoff. Tests alternate all engines at
single instructions and cover recursive budgets, aliases, cold errors, storage
growth, and promotion within one invocation.

## Observation and terminal tools

Modern module linking resolves explicit imports, visibility, trait interfaces,
and extension properties before per-body checking. Imported typed service getters
can lower directly to linked-key operations. Context views retain dense resolution
slots; provider changes update the affected key rather than invalidating caller
code. Calls use bounded per-site implementation target caches, native guards,
and stable tier entry slots. Admissions and in-flight calls root retiring
providers through cleanup and suspension. See [modules and services](services.md).

`Session::enable_reporting` enables invocation IDs, per-engine instructions/wall
time, peaks, boundary counters, native yield reasons, first-call wait, and native
preparation/materialization durations. Compiler events record request, queue,
start, finish, supersession, installation, and discarded downgrade results.
Actual entries include code version and PC. Speed OSR reports live canonical-slot
count and code-ready-to-entry latency. The latter includes installation/bridging
latency; preparation/materialization times are aggregate observed transfer costs,
not an isolated machine-frame OSR benchmark.

Helpers are charged to the calling tier; native callees choose their own engine.
OSR is an upward engine change after the same invocation has executed work.
Installation alone is not execution. Compilation overlaps execution; bounded
first-call waits are reported separately. Reporting adds overhead and caps events
at 4,096 while aggregate counters remain complete. Disabled reporting performs no
per-instruction clocks or event allocations. CLI rendering occurs afterward.

Oxc diagnostics preserve named source, related spans, Unicode/tab widths, and
terminal/plain themes. Per-stream color handling honors explicit settings and
NO_COLOR. `pretty` preserves source formatting. The REPL rechecks accepted source
plus a pending entry, reconciles globals by fixed type/name, and executes only the
new source region. Runtime errors retain prior writes. The debugger uses
interpreter statement boundaries and frame depth; Ratatui owns presentation.
