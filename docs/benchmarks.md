# Engine measurements

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

Shape-guarded property measurements and differential-fuzz coverage are documented
in [native object inline caches](native-object-ics.md). The reproduction command
is `cargo bench --bench objects --profile dev`; release timings require a separate
`cargo bench --bench objects` run.

Current engine structure and measurements are documented in
[the JIT remediation results](jit-remediation-results.md). Both eager modes now
use linked per-routine all-PC code; automatic JIT speed retains selective
entries and continuations. Native returns skip redundant synchronization.
The benchmark rotates engine order, prints effective configurations/code size,
and supports `--matched` comparisons and fresh-artifact JIT measurements.

The 2026-11-10 snapshots below describe earlier settings and execution paths.
Their relative engine rankings and flush costs are historical, not claims about
the current defaults.

## Historical measurements, 2026-11-10

The measurements below use earlier example sources. DFS and knapsack now use
zero-filled initial state, and the parser checksum now includes only parsed
tokens. Re-run `cargo bench --bench engines` for comparisons with the current
workloads; the historical timings do not measure these corrected programs.

Native arm64 macOS, Rust 1.101.0-nightly, Cranelift 0.137.1, Wasmtime/Winch 50.0.1. Recorded 2026-11-10 with `cargo bench --bench engines` (optimized build).

## Repeated-sample execution results

Each cell is the **median of 21 samples**, each sample executing **100 fresh sessions**. Parsing/checking and compilation are outside the execution interval. Tiered artifacts are explicitly prewarmed; normal execution does not wait for compilation. The benchmark freezes each requested tier and asserts the installed tiers after measurement, preventing hot-loop promotion from silently changing a Winch/fast sample into a speed sample. Session creation, initial scalar materialization, and safepoint synchronization remain inside the execution interval.

| Workload | Interpreter | Direct Cranelift fast | Winch | Tiered Cranelift fast | Tiered Cranelift speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer loops | 2.950 ms | 0.996 ms | 3.176 ms | 0.916 ms | 0.130 ms |
| Integer loop, 10,000 iterations | 229.381 ms | 67.226 ms | 294.244 ms | 69.246 ms | 7.252 ms |
| Recursive calls | 0.394 ms | 0.372 ms | 0.509 ms | 0.375 ms | 0.307 ms |
| Array indexing | 0.240 ms | 0.196 ms | 0.302 ms | 0.182 ms | 0.152 ms |
| Strings | 0.111 ms | 0.151 ms | 0.135 ms | 0.168 ms | 0.145 ms |

Actual installed tiers were checked in the benchmark output, including Winch on this ARM64 host. Tiered Cranelift speed was about **23×** the interpreter's speed on the small integer loop and **32×** on the 10,000-iteration loop. Recursive calls improved about 1.3× and array indexing about 1.6×. Strings were slower than interpretation. Winch's baseline tier currently costs more at runtime than interpreting these examples; its purpose here is baseline code generation, not a claim of peak execution speed.

Cranelift now accesses scalar slots directly through a stable VM view, emits checked operations inline, and batches PC/counter writes at safepoints. Available compiled callees use stable native entry slots. Winch retains its host imports. Allocation, call setup/return, complex values, and I/O still require helpers, which limits calls/arrays/string gains. The speed tier can optimize direct scalar memory operations much more aggressively than the fast tier. These measurements cover warm execution, not the latency of a fresh process or an asynchronous first run.

## Sized algorithm workloads

Recorded 2026-11-10 with `cargo bench --bench engines -- <filter>`. Sources are
generated from `examples/quicksort.yas`, `examples/mergesort.yas`,
`examples/fib.yas`, and `examples/knapsack.yas` by replacing their size
constants. Small/medium/large cases execute 50, 10, and 3 fresh sessions per
sample with seven samples per cell; the scalar rows above retain 100 sessions
and 21 samples. The benchmark budget is 100,000,000 bytecode instructions so
the large cases finish. Values are median sample times in milliseconds; divide
by the session count to compare per-run cost.

| Quicksort | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100 elements (50 sessions) | 33.24 | 24.21 | 19.75 | 22.09 | 18.40 |
| 1,000 elements (10) | 99.60 | 68.27 | 59.92 | 68.46 | 55.16 |
| 4,000 elements (3) | 152.27 | 106.60 | 91.22 | 104.92 | 85.35 |

| Merge sort | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100 elements (50) | 58.13 | 41.38 | 38.22 | 41.81 | 34.25 |
| 800 elements (10) | 124.58 | 93.61 | 89.17 | 92.87 | 76.20 |
| 3,200 elements (3) | 173.16 | 129.25 | 120.92 | 129.73 | 105.27 |

| Fibonacci | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fib(18)` (20) | 45.87 | 35.61 | 34.41 | 37.76 | 33.99 |
| `fib(22)` (5) | 77.89 | 59.55 | 58.17 | 63.60 | 56.21 |
| `fib(26)` (2) | 213.80 | 166.09 | 159.84 | 175.44 | 149.81 |

| Knapsack (0/1 and unbounded) | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 10 items / capacity 50 (50) | 34.93 | 29.97 | 33.80 | 29.32 | 24.75 |
| 40 items / capacity 200 (10) | 127.82 | 116.96 | 131.83 | 114.58 | 99.99 |
| 100 items / capacity 500 (3) | 252.80 | 227.70 | 257.75 | 227.99 | 192.48 |

| DFS over adjacency matrix | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100 nodes (30) | 151.87 | 64.51 | 33.85 | 63.46 | 32.69 |
| 300 nodes (5) | 227.40 | 97.38 | 52.75 | 99.20 | 51.35 |
| 600 nodes (2) | 366.82 | 160.40 | 87.25 | 158.68 | 84.63 |

| Heavy parser | Interpreter | Direct fast | Winch | Tiered fast | Tiered speed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 20 passes (20) | 587.41 | 592.04 | 581.95 | 587.79 | 553.65 |
| 50 passes (5) | 368.95 | 369.57 | 364.39 | 368.92 | 348.23 |
| 150 passes (2) | 443.48 | 448.73 | 441.81 | 443.18 | 416.26 |

Cranelift speed is the fastest engine in every sized case, and its lead grows
with the data size. Its prewarm cost is the counterweight: 10.9–17.3 ms for
quicksort, 17.5–18.1 ms for merge sort, 2.3–2.9 ms for Fibonacci, and about
24.5 ms for knapsack. On one-shot small runs those prewarm times can exceed the
saving; on medium/large or repeated runs the faster execution amortizes them.
This is why automatic tiering waits for 8,000,000 executed instructions before
requesting speed codegen and caps speed bodies at 512 bytecodes.

Winch is not consistently a baseline win, but since the fast tier adopted `Ion`
allocation it is no longer competitive with Cranelift. It still beats the
interpreter on recursive and array-heavy work and loses on knapsack, but Cranelift
fast now leads it on every sized case. Winch remains useful as a nearly
compile-free baseline (about 0.1–0.5 ms of prewarm).

Tiered fast sometimes trails direct fast on recursive workloads, for example
Fibonacci, because each routine is compiled into its own unit and calls cross
stable entry slots. Speed codegen recovers most of that gap on the sized cases.

### Recursion is limited by per-call flush, not by code quality

Recursive workloads are the one place the native tiers lose to the interpreter,
and the cause is not the generated code. Every native call site emits an
unconditional `native_flush` (`src/backend/cranelift_backend.rs`, inside
`Op::Call`) because the callee may yield in a deeper frame, so the caller must
materialize VM state before inspecting the dispatch key.

`native_flush` runs `finish_native` and `prepare_native`: it walks the frame's
dirty registers, calls `NativeSlot::materialize` on each, and then walks every
dirty cell. That is per-*call* work, independent of how many instructions the
call executes. The boundary counters show the ratio exactly:

| Workload | Invocations | `flush` boundaries |
| --- | ---: | ---: |
| fib(22) | 57,313 | 56,900 |
| quicksort | 1,349 | 1,348 |
| DFS | 1 | 0 |
| knapsack | 1 | 0 |

So `fib` pays ~1 flush per call: 785,583 instructions take 23.3 ms of native
time, or 29.7 ns/instruction, against 11.8 ns/instruction for an equivalent flat
scalar loop. DFS and knapsack, which call almost nothing at the boundary, do not
pay it at all.

This is why the tier policy cannot fix `fib`: the same flush cost applies to
Cranelift fast, Cranelift speed, and Winch, and it is why `--engine interpreter`
(18.2 ms) currently beats `--engine jit` (21.4 ms) once Winch and Cranelift
tiers are in the path. The speed-tier numbers for `fib` in the tables above are
measured with fully prewarmed, uniform-tier artifacts, which removes the
tier-transition work but not the per-call flush.

Removing the cost requires either eliding the flush when the callee provably
cannot yield, or making `finish_native` incremental so only touched slots are
materialized. Both are backend changes; no setting or threshold addresses it.

### Tier settings: allocation dominates optimization

These are not independent knobs, and the surprise is which one matters.

**Register allocation is the real lever.** Single-pass allocation spills so
heavily that it dominated everything else. Holding `opt_level=none` fixed:

| Allocator | hot scalar loop | DFS 100 | quicksort 1,000 |
| --- | ---: | ---: | ---: |
| `single_pass` (old fast tier) | 66.66 ms | 63.48 ms | 105.47 ms |
| `backtracking`/`Ion` (now) | 7.41 ms | 35.86 ms | 83.05 ms |

That is a ~9–14× difference from the allocator alone. Because of this, the old
fast tier was **dominated**: it lost to Winch on 14 of 23 workloads while
costing ~16× more to compile.

**The egraph optimizer is gated by `opt_level`.** In Cranelift 0.137.1,
`Context::optimize` runs `egraph_pass` only when `opt_level != OptLevel::None`,
and `AliasAnalysis` is constructed inside that pass. So at `opt_level=none` alias
analysis is unreachable, and it cannot be enabled separately. Turning the
optimizer on (`opt_level=speed_and_size`) buys a few percent of runtime for
about 1.4× compile time on these workloads — worth measuring, not a default.

**`enable_alias_analysis` is a dead flag.** Its generated getter is never called
anywhere in cranelift-codegen, so `true` and `false` produce identical output.
Even if it worked, this backend tags every memory access with default
`MemFlagsData`, which names no `AliasRegion`; `alias_analysis.rs` treats a
region-less store as a full fence, so there is nothing to disambiguate. Unlocking
it would require tagging VM-view/slot/cell accesses with alias regions and
inlining array/cell access instead of calling helpers.

The tier layout that follows from the measurements:

| Tier | Flags | Compile | Use |
| --- | --- | ---: | --- |
| fast | `opt_level=none`, `backtracking` | 0.6–9.6 ms | warm code; ~12% slower than speed |
| speed | `opt_level=speed`, `backtracking` | 1.5–24 ms | hot loop-only bodies |

`opt_level=speed` with `single_pass` is not a supported tier and measured worse
on both axes — about 25.6 ms vs 7.76 ms on the scalar loop, and roughly 11.2 s vs
5.9 s to compile a 2,000-statement body, because extra spill code dominates
downstream work.

Speed compile time rises sharply with body size: a 2,000-statement routine took
roughly 6 seconds to compile on this host. That is the reason
`speed_size_limit` skips speed codegen for bodies above 512 bytecodes.
Experiments override these defaults through `TierPolicy` and
`CraneliftConfig` fields; the runtime reads no environment variables.

## Compilation and startup

### Explicit Cranelift engines

A follow-up run adds `CraneliftSpeedBackend` / `--engine cranelift_speed` alongside `CraneliftBackend` / `--engine cranelift`. Both eagerly compile the whole program; their optimization settings differ. The benchmark now prints `direct-fast` and `direct-speed` separately. Execution uses the same seven-sample/100-session method above; compilation is timed separately. These numbers are from the follow-up run, rather than the preceding tier comparison.

| Workload | `cranelift` execution / 100 | `cranelift_speed` execution / 100 | `cranelift` compile | `cranelift_speed` compile |
| --- | ---: | ---: | ---: | ---: |
| Integer loops | 1.192 ms | 0.147 ms | 3.766 ms | 2.095 ms |
| Integer loop, 10,000 iterations | 67.115 ms | 5.665 ms | 0.739 ms | 1.370 ms |
| Recursive calls | 0.381 ms | 0.294 ms | 0.588 ms | 0.914 ms |
| Array indexing | 0.211 ms | 0.153 ms | 0.780 ms | 1.347 ms |
| Strings | 0.149 ms | 0.135 ms | 0.378 ms | 0.615 ms |

The speed engine executed the 10,000-iteration loop about 12× faster than the fast engine. The same run measured tiered Cranelift speed at 6.852 ms / 100; the eager speed engine avoids tier-driver quanta and used 5.665 ms / 100. This does not establish an end-to-end startup advantage: eager compilation precedes execution, including for cold routines, and the first compilation in a process can include initialization effects. The default engine remains automatic `jit`.

### Tier comparison compilation timings

The observed direct Cranelift compilation times were 2.459 ms (loops), 0.793 ms (10,000-iteration loop), 0.655 ms (calls), 0.785 ms (arrays), and 0.378 ms (strings).

Tiered prewarm wall times included queueing, the initial baseline job, any requested upgrade, instantiation, and explicit test synchronization:

| Workload | Winch prewarm | Fast prewarm | Speed prewarm |
| --- | ---: | ---: | ---: |
| Loops | 2.786 ms | 1.568 ms | 2.036 ms |
| 10,000-iteration loop | 0.687 ms | 1.347 ms | 2.112 ms |
| Calls | 0.813 ms | 0.877 ms | 1.318 ms |
| Arrays | 0.414 ms | 1.273 ms | 1.838 ms |
| Strings | 0.310 ms | 0.670 ms | 0.974 ms |

The first Winch measurement includes engine initialization. These wall times are not isolated backend compilation comparisons. Per-job worker timings and thread IDs are available through `TierInfo`/`--jit-trace=summary`. Asynchronous first-run behavior is different: normal execution interprets while code is pending and may complete before compilation finishes.

## Before-change reference

Before direct scalar storage lowering, the previous seven-sample benchmark recorded the following medians. The old examples were short enough that their requested tiers stayed installed; the new 10,000-iteration workload has no corresponding historical result.

| Workload | Interpreter | Tiered Cranelift fast | Tiered Cranelift speed |
| --- | ---: | ---: | ---: |
| Integer loops | 3.098 ms | 2.144 ms | 2.103 ms |
| Recursive calls | 0.380 ms | 0.322 ms | 0.329 ms |
| Array indexing | 0.244 ms | 0.215 ms | 0.216 ms |
| Strings | 0.102 ms | 0.106 ms | 0.102 ms |

The small-loop speed result improved about 16× against that recorded result. Helper-heavy strings regressed; direct storage adds setup/synchronization without removing most string operations. Native mirrors are lazy; pure interpreter Sessions do not allocate scalar or object-slot mirrors.

At the start of this implementation batch, single-sample totals for 100 executions were:

| Workload | Interpreter | Original direct Cranelift |
| --- | ---: | ---: |
| Loops | 5.711 ms | 5.249 ms |
| Calls | 1.190 ms | 1.215 ms |
| Arrays | 0.579 ms | 0.622 ms |
| Strings | 0.329 ms | 0.348 ms |

These were single samples, unlike the median results above, so ratios are directional rather than a controlled statistical comparison. Changes include removing full-function IR clones at entry, shared strings/constants, compact bytecode decoding, direct Cranelift branch edges, and length operations that avoid array snapshots.

Token tests separately verify a one-byte kind, a 16-byte token, no token destructor, and fewer than 32 shared-buffer allocations for more than 100,000 tokens. Bytecode words are 16 bytes, but source maps, pooled operations, metadata, and currently retained inspection IR add memory beyond that word stream.

## Collections after `StorageKind` removal, 2026-11-17

Question: did moving `Vec`/`VecDeque` to declared traits slow collection code? Method: release builds of the original
workspace (`overhaul`, before the data-enum and SDK work) and of the `data-enums` worktree, same host, run through
`cyallo run <file> --engine <engine>`; runs of the two binaries alternate; medians of 5-15 runs; the fixed cost of
`std::io::print(1)` is measured separately and subtracted. Output was identical in every comparison.

| Workload (startup-subtracted) | Engine | Baseline | Current | Ratio |
| --- | --- | ---: | ---: | ---: |
| Vec push, indexed reads x10, VecDeque push/pop both ends, 20k elements | interpreter | 115.7 ms | 118.6 ms | 1.026 |
| same, 80k elements | interpreter | 459.2 ms | 473.9 ms | 1.032 |
| same, 20k elements | jit | 50.6 ms | 51.8 ms | 1.023 |
| same, 80k elements | jit | 200.0 ms | 204.0 ms | 1.020 |
| Control: integer loops only (4M iterations), no collections | interpreter | 1727.7 ms | 1775.7 ms | 1.028 |
| Control: same | jit | 28.1 ms | 28.6 ms | 1.016 |
| Fixed startup (`print(1)`) | both | 8.5 ms | 11.3 ms | +2.8 ms |

Reading: the current tree is about 2-3% slower than the baseline on throughput and about 3 ms slower to start. The
integer-only control shows the same 1.6-2.8%, so the slowdown is **not specific to collections**: the collection
migration added no measurable collection-specific cost. The general 2-3% and the startup cost come from the earlier
work on this branch (data-enum machinery, a larger standard package and binding application); I did not bisect them.
A pre-migration snapshot of this branch does not exist, so the collection step itself cannot be isolated exactly; the
control bounds it. Not run: the Criterion `engines`/`gc`/`objects` benches (long), and an array benchmark (my source
did not compile and was dropped). Single host, single run set; differences under about 2% are within noise.

### Resolution: the 2-3% slowdown and the startup cost, 2026-11-17

There was no commit history to bisect (all of the branch's work is uncommitted), so the causes were found by measuring.
Throughput and startup had different causes.

**Throughput (2-3%): instruction and type sizes.** `size_of` in release builds, original workspace vs this branch:

| Type | Baseline | Branch before | Branch after |
| --- | ---: | ---: | ---: |
| `Type` | 48 | 56 | 48 |
| `Value` | 64 | 72 | 64 |
| `Op` | 80 | 96 | 80 |
| `Instruction` | 88 | 104 | 88 |

Three causes, each making every instruction or value bigger: `Type::EnumRef(EnumIdentity, Box<str>)` held a `Vec` inline
(now `Box<EnumIdentity>`); `Op::EnumPayload` carried a `Type` inline and was the largest `Op` (now `Box<Type>`); my
`OpaqueTraits` was 16 bools and made `Type::Opaque` 56 bytes (now a `u16` bitset with getters). The first two came from
the earlier enum work, the third from the opaque-type work. The interpreter walks `Instruction`/`Value`/`Type`, so size is
traffic.

**Startup (+2.8 ms): hashing the module-cache key.** In-process `compile("std::io::print(1)")` was 6.8 ms against 4.9 ms.
A profile of both showed `SipHasher::write` as the top frame (about 40% of samples) in both trees: `ModuleKey` held the
normalized `Debug` text of the whole native-declaration set (the entire standard package) per module, and every cache
lookup hashed it. The branch's larger native set made it longer. `ModuleKey` now holds SHA-256 digests of those texts
(`source` and `native`); compile time is 4.1 ms, below the baseline's 4.7-4.9 ms.

After both fixes (release CLI, alternating runs, medians): startup 7.9 ms against 8.4 ms; collection workloads
0.990-1.014x, the integer control 0.985-1.006x. That is parity within noise. The earlier table above records the state
before these fixes. Not run: Criterion benches; single host.

**Follow-up: no `Debug` text as identity.** `ModuleKey` was built from `format!("{:?}")` of the AST, native declarations,
signatures and types; `Debug` output is not a contract (it can change or omit fields) and `Ast` held two `HashMap`s whose
order made the text nondeterministic. The key is now a SHA-256 fingerprint of the *structure*: `Hash` is derived on the AST
(`cyallo-ast`, `services` types), real literals hash by bit pattern (`0.0` and `-0.0` differ), `Ast.source_names` and
`NativeImportPolicy.requesters` are `BTreeMap`s, and a `Hasher` adapter streams the values into SHA-256. In-process
`compile("std::io::print(1)")`: 3.7 ms, against 4.1 ms with digested Debug text and 4.9 ms for the baseline.

**Follow-up: persistent compile workers.** `cyallo-compile-jobs` created and joined a `std::thread::scope` worth of threads for
every parallel stage (parse units, function bodies, module lowering, codegen): `pthread_create`, `mmap` and `munmap`
were the top frames after the hashing fix. Stages now run on a pool of persistent workers (`rayon-core`), one pool per
configured `Jobs` width, created on first use and kept for the process; nested stages still run sequentially on their worker.
In-process `compile("std::io::print(1)")` with a warm pool: 3.2 ms (3.7 ms before, 4.8-4.9 ms baseline). One-shot CLI
startup: 7.7 ms against 8.8 ms (median of 41 alternating runs); a one-shot process still pays for creating its workers once,
so the large gain is for repeated compilation (REPL, dynamic `std::module::load`, language server, tests, embedders).

### Option and Result as general enum values, 2026-11-17

Program: 800k iterations of an `Option<i64>`-returning and a `Result<i64, i64>`-returning function, each followed by
`is_some`/`unwrap` and `is_ok`/`unwrap` (written in the syntax both trees accept; both print 8860314200). Release CLI,
original workspace (dedicated `Value::Optional`/`Value::Result`) against this branch (`Value::Enum` with reserved ids),
9 alternating runs, medians, whole-process time.

| Engine | Baseline | Branch | Ratio |
| --- | ---: | ---: | ---: |
| interpreter | 1954.3 ms | 1907.0 ms | 0.976 |
| jit | 1338.6 ms | 1273.7 ms | 0.952 |

The branch is 2-5% faster on this workload; no slowdown from the move. Single host, single run set; the Criterion
benches were not run.
