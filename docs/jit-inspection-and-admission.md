# JIT inspection, routine attributes and predictive admission

Implemented on 2026-11-12. This extends the existing
[routine optimization implementation](routine-optimization-results.md).

## Attributes

Attributes precede modern named functions or procedures, including exported and
generic declarations, implementation methods and property getter bodies.
Specialization and module linking preserve attributes and their source spans.
Anonymous closures and bodyless trait declarations do not accept them.
Unknown values, duplicate attributes and conflicting `always_lazy`/`inline(always)`
requests are syntax diagnostics.

| Attribute | Meaning |
| --- | --- |
| `#[hint(auto)]` | Ordinary automatic thresholds |
| `#[hint(hot)]` | Halve nonzero call/work thresholds, with a minimum of one; prefer predictive compilation |
| `#[hint(cold)]` | Multiply automatic thresholds by four, saturating; avoid speculative compilation |
| `#[opt(auto)]` | Predictive admission and normal tiering |
| `#[opt(eager)]` | Prefer an early job within the automatic size/work limits |
| `#[opt(lazy)]` | Suppress predictive automatic-JIT jobs; allow observed-work tiering and inlining |
| `#[opt(always_eager)]` | Bypass automatic size/work limits, retaining hard limits |
| `#[opt(always_lazy)]` | Interpret this routine; forbid native units and copied inline bodies across backends |
| `#[inline(auto)]` | Ordinary bounded inlining |
| `#[inline]` / `#[inline(preferred)]` | Larger inline budgets |
| `#[inline(never)]` | Keep calls out of line |
| `#[inline(always)]` | Strong bounded request; may embed a function or procedure with real frames |

Heat, compilation and inlining are separate decisions. `opt(lazy)` can coexist
with `inline(always)`. Selecting an explicit eager native engine requests native
compilation independently of automatic-JIT admission hints; `always_lazy` still
forbids it. Source attributes never permit stale semantics, unsupported scalar
operations, unchecked overflow, untracked GC locations or unlimited expansion.

| Inline policy | Callee words | Expanded scalar site | Depth | Added caller words |
| --- | ---: | ---: | ---: | ---: |
| Auto | 32 | 64 | 3 | 256 |
| Preferred | 64 | 128 | 3 | 512 |
| Always | 256 | 256 | 8 | 1,024 |

The frame-preserving path retains canonical VM frames, aliases, roots and exact
PCs. Its copied bodies obey callee, depth and caller limits; branches and helper
exits keep their ordinary continuation behavior. Unsupported forced requests
remain ordinary calls. `--warn-unfulfilled-attributes` reports unmet eager/forced
inline requests; normal notices alone do not enable those warnings.

## Predictive compilation

The automatic window is **[16, 128) bytecode words**. Deferred bodies first use a
cheap estimate of twice their syntax node count. A worker checks the actual
lowered bytecode size before compiling a native unit. Tiny routines favor
inlining; large routines retain observed-work admission.

Startup and first-call lookahead examine up to eight direct edges per routine,
through two breadth-first levels, with a 32-node traversal bound. Loops provide
repeated-work evidence. Shadowed local callables are never treated as statically
resolved global routines. `TieredArtifact::hint_call_soon(fi)` provides explicit
embedding-level evidence and returns a queued, already requested or deferred
decision; it retains source, size and resource gates.

Default budgets are four outstanding eager jobs, 256 cumulative estimated
speculative words and 2 ms of predicted compilation work. `always_eager` retains
the outstanding cap and a 4,096-word routine limit. Each artifact shares one
**1 ms first-call wait budget**, instead of granting every callee a fresh wait.
Timeout continues interpretation while background work remains eligible.
`TierPolicy` exposes these limits; `TierPolicy::eager.enabled = false` disables prediction.

Compile-cost histories have eight logarithmic size buckets per tier. Each keeps
sixteen samples and predicts their upper quartile. Initial estimates are 50 µs
for Winch, 500 µs for fast and 1,500 µs for speed. Full tracing and dump generation
are excluded from training. Estimates are heuristics, rather than a wall-clock
guarantee. Syntax errors remain eager; speculative semantic failures are cached
and do not fail the Session before the affected body is entered.

The native worker pool caps at three workers and reserves one logical CPU when
possible. Per-artifact atomic slots honor the compiler worker setting, including
serial mode. Immutable body analysis uses shared once-only publication.
Independent frontend stages, native units and dump formatting can run on separate
cores; nested compiler stages reuse their worker instead of multiplying pools.
Errors/results are published in source order. Installation and execution remain
on the Session thread, preserving effect order.

## Inspection

```sh
cargo run --release -- run examples/modern.ya --jit-trace=summary
cargo run --release -- run examples/modern.ya --jit-notices
cargo run --release -- run examples/modern.ya --engine cranelift_speed \
  --emit ir,bytecode,clif --emit-dir /tmp/caie-ir
```

IR files describe checked types, effects, locals/registers, instructions and
source spans. Bytecode files disassemble individual routines. Cranelift files
contain the generated function before and after optimization. Filenames include
Program snapshot, routine, native version, optimization and entry/continuation
identity as applicable. A per-snapshot manifest marks analyzed and deferred
bodies. `run` dumps only bodies actually analyzed/generated; `check` and the
`bytecode` command explicitly analyze all bodies. Requesting CLIF does not force a
Winch/interpreted routine into Cranelift. CLIF finalization drains existing native
jobs before exit so before/after files are complete; that opt-in wait contributes
to wall time, outside the VM execution interval. Emission failures produce a CLI error.

Notices distinguish inline decisions, native optimization, proactive admission,
inline guard fallback and semantic snapshot invalidation. A buffer coalesces
repeated notices and caps unique entries at 256; summary reports include overflow counts.
Workers never print; live notices drain at CLI execution safepoints. Each REPL
entry uses a separate reporting window and distinct snapshot filenames.
`--jit-trace=summary`, notices and full `--jit-trace` all preserve frameless inlining; `--jit-no-inline` disables it.

An overflow/guard miss uses the original call before committing inline effects.
A stale artifact is rejected before execution. Neither notice claims general
speculative machine-stack deoptimization. Dynamic function/procedure bindings,
trait calls and service targets remain dynamically dispatched; changed targets
cannot reuse an old inline body. General speculative dynamic inlining still
requires semantic guards and recovery maps.

## Verification and measurements

All **252 workspace/all-feature checks** and **84 targeted release checks** pass.
Formatting and strict Clippy checks are clean.

Regression coverage includes parsing/specialization/module spans, functions and
procedures in both eager layouts, forced real-frame function embedding, hard
inline limits, lazy enforcement, exact work/call counters, speculative errors,
final-size rejection, same-tier queue races, cancellation, worker allowances,
cost-history bounds, cold-body dumps, CLIF pairs, warning opt-in, overflow fallback,
stale artifacts and distinct REPL windows.

```sh
cargo test --offline --workspace --all-features
cargo test --offline --release --lib --test attributes --test eager \
  --test inspection --test inlining --test native_storage --test tiering \
  --test parallel_compilation --test cli
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo bench --offline --bench engines -- modern --admission
```

Local ARM64 macOS cold CLI measurements use three batches of 40 processes per
mode, five warmups per batch, rotating all comparison modes.
Observation is disabled. Values below are pooled median / p95 milliseconds.

| JIT workload | Saved baseline | New default | New, prediction disabled |
| --- | ---: | ---: | ---: |
| `modern.ya` | 7.691 / 8.124 | 7.518 / 8.303 | 7.401 / 8.117 |
| Flat 10,000-iteration sum | 3.719 / 4.335 | 3.617 / 4.285 | 3.665 / 4.265 |
| One scalar call | 2.954 / 3.357 | 3.002 / 3.373 | 2.955 / 3.432 |
| 100 unused routines | 3.771 / 4.377 | 3.772 / 4.248 | 3.744 / 4.290 |
| 500 unused routines | 4.545 / 5.041 | 4.570 / 5.093 | 4.570 / 5.108 |

No measured workload has a median or p95 regression above 5% repeated in all
three batches. These short workloads show stable latency, rather than evidence
of a substantial universal speedup. Cranelift speed can still cost more to compile
than fast on a cold short program; explicit compilation and warm execution remain
separate measurements. The `--admission` benchmark compares fresh artifacts with
prediction on/off while retaining the same compiler service, and includes the
frontend, backend, Session and execution interval.


On the same cold modern workload, `--jit-trace=summary` measures **7.587 / 8.164 ms**,
while intrusive `--jit-trace` measures **40.390 / 42.272 ms**. The summary retains
65,534 frameless calls; trace mode deliberately uses ordinary calls and per-call
observation.

A separate 41-sample retained-service comparison, including frontend/backend,
Session setup and execution, gives these medians:

| Workload | Prediction disabled | Prediction enabled |
| --- | ---: | ---: |
| Modern | 3.321 ms | 3.129 ms |
| Flat loop | 0.233 ms | 0.208 ms |
| 100-routine frontend | 0.375 ms | 0.379 ms |
| 500-routine frontend | 0.829 ms | 0.837 ms |

Rotated warm execution probes retain identical logical work. Modern performs
65,534 frameless calls and takes 2.260 ms (fast) / 2.055 ms (speed) per five
Sessions; the tiny call chain performs 65,536 and takes 1.606 / 1.419 ms. The
retained-frame procedure probe still shows no compelling universal runtime
benefit, so its automatic switch remains off; explicit `inline(always)` requests
can select it within the documented limits.

Raw measurements: [cold-process samples](measurements/jit-inspection-2026-11-12.json)
and [engine/admission probes](measurements/jit-inspection-engines-2026-11-12.txt).

The multicore native path also has a direct serial/default comparison: 21 rotated
cold CLI samples per configuration, including full frontend validation, native
compilation, execution and process startup. The default native unit compiler uses
three workers on this host. Outputs match in every sample.

| Routines | Engine | `--jobs 1` median / p95 | Default median / p95 |
| --- | --- | ---: | ---: |
| 100 | Fast | 24.600 / 25.795 ms | 12.834 / 13.549 ms |
| 100 | Speed | 30.520 / 32.936 ms | 15.213 / 15.821 ms |
| 500 | Fast | 107.596 / 109.030 ms | 44.352 / 45.230 ms |
| 500 | Speed | 138.029 / 141.027 ms | 55.660 / 56.961 ms |

These compilation-heavy cases improve roughly 1.9–2.5×. This is parallel compiler
work; Session execution remains sequential. [Raw native-worker measurements](measurements/jit-inspection-native-jobs-2026-11-12.txt).
