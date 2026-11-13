# Routine optimization results

Implemented on 2026-11-12, following [the routine optimization plan](function-inlining-plan.md).

The later [inspection and predictive-admission implementation](jit-inspection-and-admission.md)
adds routine attributes, compiler dumps, bounded notices and multicore compilation;
its verification and measurements are recorded separately below that contract.

## Implemented behavior

Modern `fn` rejects reads of mutable managed storage and ref captures, in addition
to mutable globals, I/O, randomness and procedure execution. Private mutation of
owned value copies remains valid. State-reading lookup helpers now use `proc`;
their boundary checks readonly effects through static callees and checks erased
method targets before execution. Deferred errors retain their original diagnostics.

Modern `const` uses checked compile-time evaluation. Known literals and frozen
aliases take a direct path; other initializers use the ordinary interpreter with
100,000 instructions, 64 frames, 16,384 cells and 1 MiB of charged value/string
allocations. Static call dependencies are checked even on untaken branches.
Executing procedures, randomness, runtime inputs and mutable identities are
rejected. Runtime code materializes frozen values without rerunning initializer
calls. Scalars become immediates; strings, records, tuples and sparse arrays can
share address-free immutable pool payloads. Mutable copies use independent rooted
storage, including during compaction. Service keys preserve declaration identity.
Named constant routine references preserve their target under local shadowing
and can become direct calls.

Cranelift fast and speed enable bounded frameless inlining for direct integer/
boolean functions and procedures. Plans use already checked immutable bodies;
cold deferred bodies stay cold. The limits are 32 reachable bytecodes per callee,
64 expanded bytecodes per site, three inline levels and 256 added bytecodes per
caller. SSA exposes nested calls and local value propagation to Cranelift.

Fuel, logical depth, initialized argument tags, reporting and callee promotion
guards run before the region. Checked arithmetic accumulates failures without
committing state. A failed guard uses the original call with its existing
arguments; no effects or work are replayed. Successful inline executions charge
each logical routine's calls and instructions separately, including readiness
and promotion accounting.

Small effectful procedure bodies have an additional opt-in path that retains
real VM frames, reference aliases, helper boundaries and GC roots. Internal
returns resume the exact caller PC, including selective speed units. Budget,
input and error exits retain the current procedure PC rather than restarting the
call. This path remains disabled by default: retained frame setup dominates its
measured runtime, while copied bodies increase compilation work and code size.

| Setting | Default | Purpose |
| --- | --- | --- |
| `CraneliftConfig.inlining` / `CAIE_INLINE_ROUTINES` | `true` / `1` | Master switch; bounded scalar function and procedure inlining |
| `CraneliftConfig.procedure_inlining` / `CAIE_INLINE_PROCEDURES` | `false` / `0` | Also embed effectful procedures while retaining frames |

`--jit-report` uses existing logical counters and preserves both optimization
paths. Intrusive `--jit-trace` / `Session::enable_reporting` disables both inline
paths and says so in its output. The original CLI used full tracing for
`--jit-report`; it was split after that observer changed measured execution.
`Session::inlined_call_count()` and `body_inlined_call_count()` provide lightweight
evidence without enabling that observer.

## Semantic replacement and remaining scope

An owned immutable Program and routine ID pin copied code and constant
dependencies. A machine-tier upgrade preserves those semantics. A new Program
rejects old native artifacts before execution; the driver must use current code
or interpretation. This is stale-artifact rejection, not general machine-stack
deoptimization.

Mutable function/procedure bindings and trait/service targets keep ordinary
dynamic dispatch. Their rebinding cannot run an old inline copy. Speculative
dynamic inlining and general mid-region deoptimization remain gated on target,
body, capture/lifetime guards and recovery maps. Captured callable constants,
constants inside closures, general pure-call CSE and loop hoisting remain future
work. The language now distinguishes immutable `let` and mutable `let mut`;
optimizer value facts must respect explicit ownership and reference contracts.

## Verification

All 224 workspace/all-feature checks pass, including the optional Wasm adapter.
The 63 targeted release checks also pass; format and strict Clippy checks are clean. New coverage exercises every budget cutoff around nested calls,
interpreter/native handoffs, global/per-routine counters, overflow and depth
fallbacks, dynamic rebinding, semantic replacement, local name shadowing,
reporting, readonly callbacks, reference aliases, forced compaction and input
suspension. Promotion regressions verify that frameless work still triggers
callee compilation at the existing thresholds.

```sh
cargo test --offline --workspace --all-features --no-fail-fast
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

## Measurements

Local ARM64 macOS release measurements. Execution comparisons use 15 rotated
samples of five fresh Sessions, holding optimization, allocator, dispatch and
resume layout fixed. Full reporting is off. A separate probe verifies logical
work and actual inline calls. Compilation comparisons use seven rotated samples
after shared compiler initialization. They exclude frontend and process startup;
cold CLI measurements include those costs.

**Default scalar inlining, execution medians (ms per five Sessions).**

| Workload | Codegen | Off | On | Speedup |
| --- | --- | ---: | ---: | ---: |
| tiny-leaves | None | 68.592 | 1.593 | 43.1× |
| tiny-leaves | Speed | 68.315 | 1.415 | 48.3× |
| modern | None | 71.933 | 2.230 | 32.3× |
| modern | Speed | 71.715 | 1.989 | 36.1× |

The tiny chain executes 1,212,429 logical instructions and 65,536 inlined calls
per Session. Modern executes 1,605,675 instructions and 65,534 inlined calls.
Inline-off and inline-on probes report identical logical work.

**Matched compilation medians (ms) and native code bytes.**

| Workload | Codegen | Compile off → on | Bytes off → on |
| --- | --- | ---: | ---: |
| tiny-leaves | None | 1.626 → 1.719 | 8,580 → 9,288 |
| tiny-leaves | Speed | 2.312 → 2.323 | 8,164 → 8,752 |
| modern | None | 2.794 → 2.919 | 14,820 → 16,064 |
| modern | Speed | 3.706 → 3.852 | 14,224 → 14,812 |

**Opt-in retained-frame procedure bodies.**

| Codegen | Default execution | Embedded execution | Compile default → embedded | Bytes default → embedded |
| --- | ---: | ---: | ---: | ---: |
| None | 32.330 ms | 32.825 ms | 1.204 → 1.563 ms | 6,128 → 7,924 |
| Speed | 32.650 ms | 32.913 ms | 1.653 → 2.085 ms | 5,852 → 6,860 |

Both execute 786,445 logical instructions per Session. The opt-in path embeds
32,768 procedure calls. Its small runtime difference does not justify the
17–29% code growth and roughly 26–30% compilation increase on this workload,
so it stays opt-in.

**Cold CLI on modern: 30 rotated samples per configuration, ms per process.**

| Engine | Inline off median / p95 | Inline on median / p95 | Median speedup |
| --- | ---: | ---: | ---: |
| jit | 17.58 / 18.54 | 6.58 / 7.22 | 2.67× |
| cranelift | 20.35 / 25.44 | 7.04 / 7.76 | 2.89× |
| cranelift_speed | 21.25 / 21.87 | 7.99 / 8.26 | 2.66× |

Cold runs use the same release binary, set `CAIE_INLINE_ROUTINES=0/1` and
`CAIE_INLINE_PROCEDURES=0`, and verify identical program output. These include
frontend, backend, Session setup and process startup. They do not use full reports.

Recorded measurements: [JSON](measurements/routine-optimization-2026-11-12.json).

```sh
cargo bench --offline --bench engines -- tiny-leaves
cargo bench --offline --bench engines -- modern
cargo bench --offline --bench engines -- procedure-calls
```

## Reporting observer correction

The original `--jit-report` selected full Session tracing, disabled inlining and
recorded every physical invocation. A user run consequently measured tens of
milliseconds of observed execution and printed over 4,000 lines. A failing CLI
regression reproduced the optimization change before the fix.

`--jit-report` now captures existing logical counters without enabling Session
tracing. It preserves inlining, includes logical work from virtual callees and
prints a bounded compiler timeline. `--jit-trace` explicitly selects the former
intrusive mode. The summary includes deferred checks, admission waits and GC in
its execution time; it does not invent per-tier/per-call phase timings.

For 21 rotated cold CLI samples on the same machine and release binary:

| Mode | Median process | p95 process | Median reported VM | Output lines |
| --- | ---: | ---: | ---: | ---: |
| Normal | 7.52 ms | 7.78 ms | — | 0 |
| `--jit-report` | 7.34 ms | 7.87 ms | 3.57 ms | 43 |
| `--jit-trace` | 31.80 ms | 32.62 ms | 27.07 ms | 4,153 |

Normal and summary times are comparable within measurement noise. Program
output, total logical work and per-routine work were verified equal. CLI tests
also cover per-entry REPL counter baselines, GC summaries and explicit tracing.
Report rendering is outside the reported VM/run intervals but inside process
measurements. [Recorded data](measurements/jit-report-2026-11-12.json).
