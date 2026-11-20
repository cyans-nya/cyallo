# JIT and engine diagnosis, 2026-11-11

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

Historical diagnosis before implementation. The confirmed issues have since
been addressed; see [the remediation results](jit-remediation-results.md) for
the final engine layout, regression coverage and current measurements.

Automatic tiering has a confirmed missed-promotion bug. The explicit engines
also use different compilation and call-dispatch structures, so their names
hide more than an optimization-level difference. On this workload the speed
engine's extra compilation and call overhead outweigh its optimization benefit.

The original release binary predates backend source changes. Measurements were
repeated after rebuilding the current sources; both binaries show the same
ordering. No production source was changed during this diagnosis.

**End-to-end measurements.** Each result is the mean of 40 fresh processes,
after five warmups, measured with `hyperfine`. Output is captured rather than
rendered in a terminal. Compilation, execution, and process startup are included.

| `modern.ya` mode | Original binary | Rebuilt binary |
| --- | ---: | ---: |
| interpreter | 29.8 ms | 34.8 ms |
| jit | 16.8 ms | 19.8 ms |
| cranelift | 18.3 ms | 22.0 ms |
| cranelift_speed | 24.3 ms | 27.9 ms |
| jit with `--jit-trace=summary` | 33.5 ms | 36.7 ms |

Compare engines within a column. The original comparison included reporting
only for JIT; reporting approximately doubles its latency here. Without
reporting, JIT is the fastest of these CLI modes on this example.

Reproduction:

```sh
hyperfine --warmup 5 --runs 40 \
  './target/release/cyallo run examples/modern.ya --engine interpreter' \
  './target/release/cyallo run examples/modern.ya --engine jit' \
  './target/release/cyallo run examples/modern.ya --engine cranelift' \
  './target/release/cyallo run examples/modern.ya --engine cranelift_speed' \
  './target/release/cyallo run examples/modern.ya --engine jit --jit-trace=summary'
```

**Confirmed bug: native execution can miss the fast-tier threshold.**

`TierPolicy::promote` selects fast at 5,000 executed instructions, but it is
called only after control returns to `TieredArtifact::run`. The native control
table populated by `State::poll` contains the 8,000,000-instruction speed
threshold, or `u64::MAX` for routines ineligible for speed. It never contains
the next Winch-to-fast threshold.

The native polls check this limit, OSR requests, and compiler-completion epochs.
The 1,024-instruction poll interval checks for notifications; it does not
periodically return to the tier driver to reconsider promotion. With no new
notification, native code can consume the whole outer execution budget.

Relevant code: `src/backend/tiered.rs:301`, `:524`, and
`src/backend/cranelift_backend.rs:512`. The CLI supplies a 1,000,000-instruction
outer budget at `src/main.rs:411`.

A deterministic probe precompiles only Winch, then executes this program in
one `artifact.run(..., 100_000_000)` call:

```text
let mut total = 0
for i = 1 to 100000 { total += i }
std::io::print(total)
```

It checks that fast compilation was requested after the warm threshold was
crossed. All five runs fail with:

```text
work=2100013; warm threshold=5000; installed=Some(Winch); requested=Some(Winch)
assertion failed: native execution missed the fast-tier threshold
```

Precompilation makes this probe independent of worker timing. Ordinary automatic
runs exhibit the same scheduling problem: in an unobserved Session with only
compiler events recorded, `modern.ya` requests fast for main at 693,895
instructions, cube at 163,360, and square at 150,262. Raising the outer budget
from 1,000,000 to 100,000,000 leaves all three installed in Winch through
completion in that run. Exact first-run request counts depend on compiler timing.

The existing nine tiering tests pass. They test policy selection and speed OSR,
but do not catch a native Winch invocation crossing a nonzero fast threshold.

**Engine comparison: optimization and execution structure are coupled.**

`CraneliftBackend::compile` builds one combined artifact with
`compile_unit(program, None, false)`. `CraneliftSpeedBackend::compile` builds a
separate artifact for every routine and links them through native entry slots.
Each speed artifact additionally compiles an optimized continuation part.
See `src/backend/cranelift_backend.rs:256` and `:797`.

After an indirect native call, the generated caller unconditionally invokes
`native_flush` (`src/backend/cranelift_backend.rs:1061`). The flush finishes
materializing the current frame and prepares its native view again. The
combined fast artifact uses its internal VM dispatch and does not take this
extra cross-artifact flush path in this example.

Nine fresh reporting runs per engine gave median backend setup times of
3.703 ms for fast and 8.708 ms for speed. Boundary counts were stable:

| Engine | `step` boundaries | `flush` boundaries |
| --- | ---: | ---: |
| cranelift | 131,088 | 0 |
| cranelift_speed | 131,088 | 65,535 |

These setup timings and boundary counts explain behavior; reported execution
durations are instrumented and are not substituted for unobserved benchmarks.

**Separating compilation from execution preserves a smaller call-heavy loss.**

A temporary Rust harness compiles and prewarms artifacts before measuring.
It rotates engine order over 15 samples, with five fresh Sessions per sample.
Values below are median milliseconds per Session. Session setup and runtime
synchronization are included; parsing, checking, and compilation are excluded.
Tiered artifacts have automatic promotion disabled during this measurement.

| Artifact | modern.ya | Flat sum loop, 10,000 iterations |
| --- | ---: | ---: |
| combined fast | 15.119 | 0.092 |
| eager speed | 15.942 | 0.075 |
| per-routine Winch | 16.386 | 0.127 |
| per-routine fast | 16.102 | 0.084 |
| per-routine speed | 15.935 | 0.071 |
| interpreter | 32.221 | 3.228 |

The per-routine fast and speed paths are close on the call-heavy example;
both trail combined fast. Speed improves the flat loop, confirming that the
ordering is workload dependent. Extra compilation explains much of the CLI
gap, while call structure adds an execution penalty even after compilation.

Removing strings, builtins, continue, and break from `modern.ya` preserves
the slowdown: the original binary takes 15.8 ms with fast and 19.3 ms with speed
over 20 CLI runs. Calls plus arithmetic are sufficient to reproduce it.

**The optimization flags are assigned as intended.**

Both tiers default to the backtracking register allocator. Fast defaults to
`opt_level=none`; speed uses `opt_level=speed`. No relevant environment overrides
were set. Enabling `CAIE_FAST_OPTIMIZE=1` preserves combined fast's advantage on
the call-heavy example: warm combined fast measures 14.929 ms and eager speed
16.031 ms in that experiment. This override uses `speed_and_size`, so it is a
controlled experiment on enabling fast optimization, not an exact equivalence
of flags. Swapping the engine names or optimization flags would not address
the observed dispatch and compilation costs.

Speed is deliberately restricted to hot loop-only routines by automatic policy.
Main and cube contain calls, while square has no loop, so zero automatic speed
instructions on `modern.ya` is expected under that policy. The policy's
whole-routine classification is conservative; it cannot optimize a hot loop
region separately from calls elsewhere in the routine.

**Recommended implementation order.**

1. Make native limits represent the next applicable tier transition, including
   Winch-to-fast, and cover nested native callees. Add the deterministic
   threshold-crossing regression at the Session/artifact seam.
2. Compare fast and speed using the same compilation scope, dispatch ABI, and
   continuation strategy before tuning flags or advertising an engine ordering.
3. Reduce repeated call synchronization with an explicitly safe call/return
   contract or incremental materialization. Deleting the flush unconditionally
   would break the deeper-frame yield contract it currently protects.
4. Recalibrate admission thresholds with separate cold CLI, warm execution, and
   compilation measurements across scalar loops and call-heavy workloads.

Validation: all 27 reporting executions matched the original program output;
all nine existing release tiering tests passed; the focused missed-promotion
assertion failed consistently in five runs. The failure is retained as evidence;
production fixes were outside this diagnosis.

Raw benchmark JSON, reports, and the temporary Rust probes are retained under
`/tmp/caie-diagnosis-*` on this machine.
