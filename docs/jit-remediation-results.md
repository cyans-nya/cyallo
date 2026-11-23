# JIT remediation results

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

The [remediation plan](jit-remediation-plan.md) is implemented. Measurements
below compare the release binary preserved before implementation with the final
release binary on the same ARM64 macOS host. The [original diagnosis](jit-diagnosis-2026-11-11.md)
records the behavior before these changes.

**Correctness and execution changes.**

- Native control limits now represent the next unrequested tier transition.
  Winch loops and repeatedly called native leaves request fast at their warm
  threshold, including when called from an active native caller. Requested and
  installed tiers jointly disable obsolete limits, including failed requests
  and Winch fallback. Size-rejected speed upgrades cannot repeatedly yield.
- Both eager engines now compile per-routine units with all-PC resume entries
  and link the same native entry table. Fast defaults to `opt_level=none` and
  speed to `opt_level=speed`; both default to backtracking allocation. Neither
  eagerly builds selective-entry continuations. Automatic JIT retains selective
  speed entries and native continuations.
- `CraneliftConfig` separates optimization, compilation scope, resume strategy,
  and allocation. Combined dispatch remains available for explicit experiments.
  `CraneliftArtifact` and `EagerSpeedArtifact` remain compatibility names for
  the common eager artifact. Benchmarks print effective configurations and code
  size; reports print effective configurations and actual admission conditions.
- Native return synchronization skips `finish_native`/`prepare_native` when
  the callee has already materialized state and refreshed the caller view.
  Pending instructions, dirty cells, and errors still require synchronization.
  Debug assertions check the corresponding clean-register invariant. Reports
  distinguish flush attempts, actual work, and skipped work.
- Eager execution disables compiler-readiness polling because it has no
  compiler mailbox. Tier thresholds and exact instruction budgets remain
  independent of readiness polling.

The final `modern.ya` trace requests fast at main work **5,001**, cube
**5,000**, and square **5,005**, instead of waiting for the outer million-word
budget. Both eager traces show 65,535 native return synchronization attempts,
all already clean and skipped on this run. Separate seven-instruction-budget
tests prove dirty callee yields still take the synchronization path.

**Cold CLI latency.**

Three batches of 40 measured processes per mode, each after five warmups,
without observation enabled. Values are pooled median / p95 milliseconds over
120 processes. The original and final binaries were measured in each batch;
absolute timings should not be compared with measurements from another batch
or another machine.

| `modern.ya` mode | Before median / p95 | After median / p95 |
| --- | ---: | ---: |
| JIT | 17.44 / 17.76 | 16.80 / 17.06 |
| Eager fast | 19.03 / 20.24 | 19.06 / 19.59 |
| Eager speed | 24.62 / 25.03 | 19.95 / 20.28 |

JIT improves about 4%; eager fast stays approximately unchanged; eager speed
improves about 19%. Speed still has higher cold compilation cost than fast on
this short call-heavy program. This is an explicit tradeoff rather than a
dispatch/continuation mismatch. Final JIT with reporting enabled measures
28.63 / 29.10 ms; reporting remains opt-in and is excluded from performance
claims.

**Warm execution.**

The updated benchmark rotates engine order across samples. Compilation and
front-end work are outside these intervals; Session creation and synchronization
are included. These are final-build medians in milliseconds per Session,
normalized by the sample's Session count. Small standard fixtures use 21
samples, modern/tiny-leaf workloads 15, and sized algorithms seven.

| Workload | Interpreter | Eager fast | Eager speed |
| --- | ---: | ---: | ---: |
| Sum loop, 10,000 iterations | 3.252 | 0.0803 | 0.0712 |
| modern.ya | 30.59 | 14.67 | 14.54 |
| Minimal square/cube calls | 24.55 | 13.39 | 13.37 |
| Quicksort, 1,000 elements | 10.44 | 5.68 | 5.63 |
| Merge sort, 800 elements | 12.70 | 7.97 | 7.92 |
| Fibonacci, fib(22) | 19.31 | 14.49 | 14.41 |
| Knapsack, 40 items / capacity 200 | 15.62 | 10.89 | 10.84 |
| DFS, 300 nodes | 148.80 | 52.11 | 51.73 |
| Parser, 50 passes | 74.07 | 67.09 | 66.50 |

The flat loop benefits from optimization; calls and helper-heavy workloads
show smaller differences. The complete benchmark also covers small/large
algorithm sizes, arrays, strings, and 100/500-routine code-generation cases.
Fresh-artifact JIT samples create new artifacts, retain the compiler service,
and explicitly exclude the front end; they are labeled separately from true
cold-process measurements.

**Why the final eager layout is per routine.**

Combined all-PC dispatch was the first candidate and removed the old speed
engine's duplicated continuation compilation. Its large compiler graph raised
peak memory to roughly 111 MiB for 500 tiny routines. The matched scope/resume
matrix showed that per-routine all-PC code retained comparable warm execution
after redundant return synchronization was removed. It also compiled the
500-routine speed case faster than combined dispatch in the matched run.

The final independent-process scaling check uses 500 declared tiny routines,
one called from main. Five measured processes per configuration after one
warmup; values are mean wall time and mean peak RSS. This is a synthetic eager
compilation check, not a claim about every large program.

| Engine | Before wall / peak RSS | Final wall / peak RSS |
| --- | ---: | ---: |
| Eager fast | 109.0 ms / 100.7 MiB | 97.8 ms / 21.7 MiB |
| Eager speed | 219.6 ms / 30.9 MiB | 126.6 ms / 22.1 MiB |

Per-routine entry scaffolding can increase emitted code size compared with a
combined dispatcher. The selected layout favors bounded compiler graphs,
measured cold latency and peak memory, and a consistent eager call contract.
The benchmark records emitted code bytes so this tradeoff remains visible.

**Admission calibration.**

The fast threshold remains **5,000**, baseline admission remains **8 calls or
2,000 instructions**, and the speed threshold remains **8,000,000** with the
existing loop-only and size restrictions. The scheduler fix was verified with
unchanged thresholds before calibration.

A seeded, interleaved calibration compared fast thresholds of 5,000, 50,000,
500,000, 2,000,000, and effectively disabled fast promotion. Each setting used
15 cold samples after warmup across nine workloads. Raising the threshold
helped some short programs but hurt others; disabling fast was about 8% slower
on DFS. No candidate established a universal replacement. Representative
median milliseconds from that calibration, which held the JIT structure fixed:

| Workload | 5,000 | 50,000 | 2,000,000 | Fast disabled |
| --- | ---: | ---: | ---: | ---: |
| modern.ya | 19.34 | 19.55 | 19.75 | 19.45 |
| Flat 10,000-iteration loop | 3.95 | 3.87 | 3.73 | 3.87 |
| Quicksort | 10.98 | 10.84 | 10.63 | 10.57 |
| Knapsack | 17.77 | 17.44 | 18.28 | 18.40 |
| DFS | 59.34 | 60.32 | 60.90 | 64.10 |

Break-even remains workload dependent. The flat loop's measured Winch-to-fast
gain is only about 0.04 ms per 10,000 iterations against roughly 1.3 ms of fast
prewarming, so a single short loop cannot recover that compiler work. DFS
offers milliseconds of execution savings, while modern's tiny calls show much
less difference between fixed tiers. Compilation overlaps execution in JIT;
prewarm time must not simply be added to JIT wall time. Retaining the current
heuristics avoids overfitting one example; it does not guarantee every requested
compilation pays for itself.

**Verification and reproduction.**

- `cargo test --workspace`: 202 passing checks, including doctests.
- Targeted release library/runtime suite: 73 passing checks. These overlap
  workspace coverage and validate optimized builds separately.
- Native handoffs compare output, status, instruction counts, spans, stacks,
  variables and globals against interpretation. Coverage includes budgets of
  one instruction, 300-deep recursion through both eager defaults, BYREF aliases,
  input, errors, GC stress, mixed tiers, and 12 explicit compiler configurations.
- Regression tests cover threshold requests with and without observation,
  nested callers, coalesced requests, failed upgrades, rejected speed sizes,
  clean return fast paths, and dirty suspended-frame synchronization.
- `cargo fmt --all --check` and the complete engine benchmark pass.

```sh
cargo test --workspace
cargo test --release --test tiering --test engines --test native_emitters \
  --test native_storage --test metrics --test deferred --test gc --lib
cargo bench --bench engines
cargo bench --bench engines -- modern --matched
cargo bench --bench engines -- hot-loop --matched
cargo bench --bench engines -- codegen-size --matched
hyperfine --warmup 5 --runs 40 \
  './target/release/cyallo run examples/modern.yas --engine jit' \
  './target/release/cyallo run examples/modern.yas --engine cranelift' \
  './target/release/cyallo run examples/modern.yas --engine cranelift_speed'
```

Raw traces and benchmark outputs remain in `/tmp/caie-remediation-*` on this
machine. Region JIT, speculative inlining and a new native calling convention
remain outside this remediation's scope.
