# Startup baseline

Measurement slice of [data-oriented-ir-design.md](data-oriented-ir-design.md#migration-from-the-current-body-pipeline).
Revision `d1c62f5` plus the instrumentation in this change; aarch64 macOS, 16 CPUs, release
profile, shared with other sessions' builds (so p95 carries noise; medians are the claim).

**Verdict.** Time to first output grows linearly with the number of declared routines and with
their total size, at about **80 µs per 8-statement routine** (`--jobs 1`; `--jobs 0` saves only
~10%). The fixed cost with no user routines is ~3.7 ms in process (7 ms from launch). No lazy
stage can win at all until the *eager* per-body passes shrink, and the largest of them is not on
the design's list:

| Rank at n=4096 (328 ms in process) | Stage | ms | Share | Scales with |
| ---: | --- | ---: | ---: | --- |
| 1 | **module cache keys** (`module_cache::keys`, in `module_analysis`) | 134 | 41% | body size × routine count |
| 2 | name resolution (`link_with_core`) | 61 | 19% | routines, superlinear (×7.4 for ×4 routines at 16384) |
| 3 | lex + parse + freeze + publish | 41 | 12% | body bytes |
| 4 | generic specialization | 33 | 10% | body size |
| 5 | constant evaluation walk | 22 | 7% | body size |
| 6 | declarations, deferred setup, `requires_tracing`, link | 28 | 9% | routines |
| 7 | linker materialization | 9 | 3% | body size |
| – | bytecode (module wrapper only) | 0.4 | 0.1% | routines |

Bytecode translation and checked-body analysis cost **nothing** before the first output: 0 bodies
are checked and 1 is translated (the entry), for any N. Lazy bytecode emit and lazy AIR are
already in effect for startup; they pay only after first output (below). Everything above is the
frontend's whole-program eager work, repeated over each body five or six times.

## What this implies for the order

1. **Remove or defer cache-key computation (not currently in the plan).** `keys()` clones every
   body (`body.to_vec()`), walks it, and SHA-256-hashes the module. The cache it feeds is a
   per-process `ModuleCache` (no persistent compiler cache exists at this revision), so a fresh
   process never hits it. This is 41% of startup for free-standing code and scales with body size
   (12 ms at 2 statements to 447 ms at 128, n=1024).
2. **Indexed-syntax resolution** (resolve + materialize + the clones). Bodies exist as an owned
   tree, an indexed freeze, a materialized tree, and then clones in `keys`, `requires_tracing`
   and `DeferredBody` (at least 5 copies after parse: counters `bodies_frozen`,
   `bodies_materialized`, `bodies_tracing_copied`, `bodies_deferred_copied`, plus the key clone).
   Resolve, specialize, const-eval and materialize total 124 ms (38%), all body-size-driven.
   Direct indexed construction (the design's "freeze dominates" trigger) is *not* indicated:
   freeze is 11 ms (3%) against parse 25 ms.
3. **IR compaction.** No startup case before first output (no checked IR exists then). It matters
   only after the first call; see the all-routines row.
4. **Lazy bytecode emit and lazy AIR as startup work: no gain.** Already deferred. Their value is
   the *first-call and total* cost below, where they cannot reduce the work but can stop
   `header_clone` from making it quadratic.

## Work scaling with routine count and size

Medians of 25 paired rounds, `--jobs 1`; "Δ" is paired against the n=0 control from the same round.

| Factor varied | Values | First output (ms) |
| --- | --- | --- |
| Unused routines (body 8) | 0 / 256 / 1024 / 4096 / 16384 | 7.0 / 25.1 / 82.0 / 326.5 / 1528 |
| Statements per routine (n=1024) | 2 / 8 / 32 / 128 | 35.5 / 82.0 / 257 / 970 |
| Files holding the routines (n=1024) | 0 / 4 / 16 / 64 | 82.0 / 79.1 / 80.8 / 82.1 |

- Count and size both scale linearly (≈ 80 µs/routine at 8 statements; ≈ 7 µs per statement per
  routine). Per-routine fixed cost is small: b2 costs 35 ms for 1024 routines.
- File/import count has no measurable effect here. The sweep adds one `use` per file, so it varies
  files, not resolved import items; a heavier import sweep remains open.
- Memory: peak live bytes are ~52 KB per 8-statement routine (53 MB at n=1024, 839 MB at 16384),
  and 220 KB allocated per routine. Memory reduction targets the same copies.
- Parallelism (`--jobs 0`, 16 CPUs) reduces first output by only 7–11% (n=4096: 292 vs 326 ms);
  the serial stages dominate.
- Body counts for a fixed print-only entry: every declared routine is parsed, frozen,
  materialized, resolved, copied into a deferred body and cloned by `requires_tracing`
  before first output (`bodies_* = N+2`), but 0 are checked and 1 translated.

## Forced work: first call and call-everything

Each workload prints `ready`, then waits for a stdin line, then acts, so first-output and
post-output work separate cleanly (default `jit` engine, `--jobs 1`).

| Workload | First output | After output (call) | Bodies checked / translated after first output |
| --- | ---: | ---: | --- |
| `unused_big` n=1024 (never called) | 93.4 | 2.5 | 0 / 0 |
| `call_one` n=1024 (1500-statement routine) | 92.5 | **739.7** | 1 / 1 |
| `call_one` n=4096 | 340.8 | 738.1 | 1 / 1 |
| `call_all` n=256 | 26.9 | 30.4 | 257 / 257 |
| `call_all` n=1024 | 89.1 | **503** | 1025 / 1025 |
| `call_all` n=4096 | 360.5 | **2564** (p95 5210) | 4097 / 4097 |

- The forced large routine costs the same at n=0, 1024 and 4096 (≈740 ms): independent of
  unused-routine count. Of that, frontend body analysis is only ~14 ms (lowering 12.3, metadata
  1.4) and bytecode translation 0.04 ms; the rest is the default `jit` engine preparing the
  routine (`--engine interpreter` finishes the whole program in ~110 ms). The backend, not the
  frontend, dominates first call of a large body. `call_one` n=4096 wait: exit minus first
  output includes this.
- **Calling every routine is quadratic.** Per forced body at n=256 / 1024 / 4096 the analysis costs
  0.10 / 0.18 / 0.42 ms. `header_clone` (cloning the whole header vector for the local effect
  check) is 2.6 / 50 / 769 ms total and `body_effects` (allocating per-function vectors) 0.6 / 9 /
  128 ms. Header elements cloned: 66,563 / 1,052,675 / 16,793,603 (≈ N²). At n=4096 allocation
  is 30.8 GB and peak live 1.7 GB, against 0.9 GB allocated for the unused program.
  The design's "cloning all headers once per forced body scales with both counts" holds.
  Interpreter-only end to end for `call_all` n=4096 is 1.85 s, so the frontend alone is ≥ 1.5 s
  of the 2.9 s.
- Work moved across the first-output boundary is therefore *small* for the unused case
  (nothing is checked before first output), large when forced, and its growth is the header
  clone. Sharing header contracts (the "share deferred analysis inputs" slice) removes the
  quadratic term (~0.9 s of 2.6 s at n=4096) and is independent of the startup ranking above.

## What was not measured

- Cold filesystem cache, `--jobs` scaling beyond 1 vs all cores, warm module reuse
  (`ModuleCache` hits), rebuild-after-edit and parallel full-check workloads, and operand arity
  histograms. Peak live bytes are reported only for the whole process, not per stage.
- Allocation per stage is exact only at `--jobs 1`; stage times are inclusive and nested.
- `bodies_parsed` and the other counts include bodyless declarations (the embedded core package
  contributes 62 parsed routines, parse units 197 and 28 files at n=0); `bodies_resolved` is
  the resolver's output, in which core declarations are mostly absent (2 at n=0).
- There is no persistent compiler cache at this revision, so "empty cache" holds trivially.

## How it was measured

- `crates/cyallo-startup-profile` provides stage timers, counters, a counting allocator and the
  first-output snapshot. Without cargo feature `startup-profile` every call is an empty inline
  function (zero cost); the timing runs use a build *without* it, so they carry no
  instrumentation. With the feature, `CYALLO_STARTUP_PROFILE=1` (text) or `=json` prints the
  report on stderr. The crate holds process-wide write-only statics: an explicit, documented
  exception to the "no process-wide counters" rule, scoped to measurement builds and never read
  by compilation or execution.
- Instrumented sites: lex and unit split, parse, freeze, publish (`units.rs`); core package parse;
  materialize (`dynamic_modules.rs`); resolve; specialize and const-eval (`compiler.rs`);
  analysis sub-stages (`analysis/modules.rs`); forced body, lowering, header clone, effects,
  finish (`analysis.rs`); bytecode module and per-routine translation (`bytecode.rs`).
  First output is marked in `main.rs` after the first flush of program output.
- `scripts/startup_baseline.py`: `bench` (randomized paired fresh-process runs, per-round shuffled
  order including the control, first byte time via pipe, median/p95), `profile` (instrumented
  medians), `report` (Markdown from the saved JSON), `generate` (the committed
  [examples/startup](../examples/startup/README.md) set). Raw data:
  [bench](measurements/startup-baseline-bench.json),
  [profile](measurements/startup-baseline-profile.json).

```sh
cargo build --release
cargo build --release --features startup-profile --target-dir target-profile
uv run scripts/startup_baseline.py bench --reps 25 --jobs 1,0 --out docs/measurements/startup-baseline-bench.json
uv run scripts/startup_baseline.py profile --reps 7 --out docs/measurements/startup-baseline-profile.json
```

## Appendix: full tables

Generated by `uv run scripts/startup_baseline.py report`. Stage times are median milliseconds,
inclusive; indented rows are parts of the row above (`resolve` includes core package parse).
Workload abbreviations: `unu` unused, `ubig` unused_big, `one` call_one, `all` call_all; `n` routines, `b` statements per routine, `m` extra files.

#### Stages: unused-routine count

| median ms | unu n=0 b8 m0 | unu n=256 b8 m0 | unu n=1024 b8 m0 | unu n=4096 b8 m0 | unu n=16384 b8 m0 |
|---|---:|---:|---:|---:|---:|
| lex | 0.07 | 0.34 | 1.07 | 3.71 | 15.08 |
| parse | 2.73 | 4.15 | 7.88 | 25.28 | 94.05 |
| freeze | 0.02 | 0.74 | 2.83 | 11.03 | 45.08 |
| publish | 0.00 | 0.06 | 0.20 | 0.78 | 3.12 |
| materialize | 0.01 | 0.58 | 2.21 | 9.44 | 38.27 |
| resolve | 2.29 | 4.57 | 12.60 | 60.81 | 447.66 |
|   core_package_parse | 1.51 | 1.49 | 1.25 | 1.38 | 1.39 |
| specialize | 0.13 | 2.14 | 8.32 | 33.18 | 137.17 |
| const_eval | 0.09 | 1.37 | 5.32 | 21.86 | 87.85 |
| module_analysis | 0.64 | 10.68 | 40.23 | 158.16 | 651.83 |
|   module_declarations | 0.06 | 0.65 | 2.43 | 10.14 | 47.77 |
|   module_keys | 0.40 | 8.95 | 33.85 | 133.75 | 544.90 |
|   module_body_wave | 0.06 | 0.49 | 1.66 | 6.52 | 26.35 |
|     deferred_setup | 0.00 | 0.39 | 1.46 | 6.01 | 24.50 |
|     entry_lowering | 0.05 | 0.09 | 0.17 | 0.37 | 1.28 |
|   requires_tracing | 0.00 | 0.34 | 1.66 | 5.63 | 22.08 |
|   module_link | 0.01 | 0.37 | 1.75 | 5.94 | 23.98 |
| bytecode_module | 0.00 | 0.02 | 0.06 | 0.43 | 1.80 |
| body_analysis | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_lower | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
|   header_clone | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_effects | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_finish | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| bytecode_translate | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| time to first output | 3.65 | 22.61 | 80.41 | 328.55 | 1533.65 |
| time to exit | 3.70 | 22.66 | 80.46 | 328.62 | 1533.80 |
| peak live MB | 1.4 | 14.1 | 53.2 | 209.9 | 838.9 |
| allocated MB (total) | 4.4 | 59.8 | 226.9 | 895.8 | 3578.1 |
| bodies_parsed (first out / exit) | 62 / 62 | 318 / 318 | 1086 / 1086 | 4158 / 4158 | 16446 / 16446 |
| bodies_frozen (first out / exit) | 0 / 0 | 256 / 256 | 1024 / 1024 | 4096 / 4096 | 16384 / 16384 |
| bodies_materialized (first out / exit) | 0 / 0 | 256 / 256 | 1024 / 1024 | 4096 / 4096 | 16384 / 16384 |
| bodies_resolved (first out / exit) | 2 / 2 | 258 / 258 | 1026 / 1026 | 4098 / 4098 | 16386 / 16386 |
| bodies_deferred_copied (first out / exit) | 2 / 2 | 258 / 258 | 1026 / 1026 | 4098 / 4098 | 16386 / 16386 |
| bodies_tracing_copied (first out / exit) | 2 / 2 | 258 / 258 | 1026 / 1026 | 4098 / 4098 | 16386 / 16386 |
| bodies_checked (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| bodies_translated (first out / exit) | 1 / 1 | 1 / 1 | 1 / 1 | 1 / 1 | 1 / 1 |
| header_elements_cloned (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

#### Stages: body size (statements), n=1024

| median ms | unu n=1024 b2 m0 | unu n=1024 b8 m0 | unu n=1024 b32 m0 | unu n=1024 b128 m0 |
|---|---:|---:|---:|---:|
| lex | 0.45 | 1.07 | 2.85 | 10.19 |
| parse | 4.61 | 7.88 | 22.22 | 77.35 |
| freeze | 1.45 | 2.83 | 8.49 | 28.71 |
| publish | 0.14 | 0.20 | 0.46 | 1.48 |
| materialize | 0.81 | 2.21 | 8.16 | 30.87 |
| resolve | 7.07 | 12.60 | 32.77 | 111.90 |
|   core_package_parse | 1.40 | 1.25 | 1.43 | 1.33 |
| specialize | 2.99 | 8.32 | 26.51 | 98.68 |
| const_eval | 1.84 | 5.32 | 16.84 | 63.88 |
| module_analysis | 15.10 | 40.23 | 135.78 | 518.19 |
|   module_declarations | 0.99 | 2.43 | 7.38 | 28.89 |
|   module_keys | 12.21 | 33.85 | 117.02 | 446.62 |
|   module_body_wave | 0.83 | 1.66 | 4.99 | 18.13 |
|     deferred_setup | 0.66 | 1.46 | 4.80 | 17.93 |
|     entry_lowering | 0.14 | 0.17 | 0.15 | 0.16 |
|   requires_tracing | 0.47 | 1.66 | 4.95 | 20.54 |
|   module_link | 0.56 | 1.75 | 5.06 | 20.66 |
| bytecode_module | 0.05 | 0.06 | 0.11 | 0.12 |
| body_analysis | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_lower | 0.00 | 0.00 | 0.00 | 0.00 |
|   header_clone | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_effects | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_finish | 0.00 | 0.00 | 0.00 | 0.00 |
| bytecode_translate | 0.00 | 0.00 | 0.00 | 0.00 |
| time to first output | 32.65 | 80.41 | 255.79 | 951.44 |
| time to exit | 32.70 | 80.46 | 255.84 | 951.49 |
| peak live MB | 22.7 | 53.2 | 175.7 | 666.6 |
| allocated MB (total) | 96.5 | 226.9 | 745.8 | 2840.0 |
| bodies_parsed (first out / exit) | 1086 / 1086 | 1086 / 1086 | 1086 / 1086 | 1086 / 1086 |
| bodies_frozen (first out / exit) | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 |
| bodies_materialized (first out / exit) | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 |
| bodies_resolved (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_deferred_copied (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_tracing_copied (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_checked (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| bodies_translated (first out / exit) | 1 / 1 | 1 / 1 | 1 / 1 | 1 / 1 |
| header_elements_cloned (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

#### Stages: import count (files), n=1024

| median ms | unu n=1024 b8 m0 | unu n=1024 b8 m4 | unu n=1024 b8 m16 | unu n=1024 b8 m64 |
|---|---:|---:|---:|---:|
| lex | 1.07 | 0.98 | 1.00 | 0.93 |
| parse | 7.88 | 7.53 | 8.27 | 8.36 |
| freeze | 2.83 | 2.74 | 2.88 | 2.92 |
| publish | 0.20 | 0.20 | 0.21 | 0.22 |
| materialize | 2.21 | 2.23 | 2.43 | 2.28 |
| resolve | 12.60 | 10.55 | 11.23 | 10.75 |
|   core_package_parse | 1.25 | 1.26 | 1.36 | 1.35 |
| specialize | 8.32 | 7.31 | 8.15 | 7.17 |
| const_eval | 5.32 | 4.61 | 5.04 | 4.71 |
| module_analysis | 40.23 | 37.30 | 38.13 | 38.75 |
|   module_declarations | 2.43 | 2.22 | 2.27 | 2.27 |
|   module_keys | 33.85 | 31.31 | 32.03 | 32.55 |
|   module_body_wave | 1.66 | 2.01 | 1.90 | 1.93 |
|     deferred_setup | 1.46 | 1.83 | 1.70 | 1.74 |
|     entry_lowering | 0.17 | 0.15 | 0.16 | 0.16 |
|   requires_tracing | 1.66 | 1.32 | 1.32 | 1.34 |
|   module_link | 1.75 | 1.40 | 1.41 | 1.43 |
| bytecode_module | 0.06 | 0.06 | 0.07 | 0.06 |
| body_analysis | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_lower | 0.00 | 0.00 | 0.00 | 0.00 |
|   header_clone | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_effects | 0.00 | 0.00 | 0.00 | 0.00 |
|   body_finish | 0.00 | 0.00 | 0.00 | 0.00 |
| bytecode_translate | 0.00 | 0.00 | 0.00 | 0.00 |
| time to first output | 80.41 | 72.74 | 77.01 | 77.60 |
| time to exit | 80.46 | 72.80 | 77.05 | 77.65 |
| peak live MB | 53.2 | 39.9 | 39.9 | 40.3 |
| allocated MB (total) | 226.9 | 225.8 | 225.2 | 222.1 |
| bodies_parsed (first out / exit) | 1086 / 1086 | 1086 / 1086 | 1086 / 1086 | 1086 / 1086 |
| bodies_frozen (first out / exit) | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 |
| bodies_materialized (first out / exit) | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 | 1024 / 1024 |
| bodies_resolved (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_deferred_copied (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_tracing_copied (first out / exit) | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 | 1026 / 1026 |
| bodies_checked (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| bodies_translated (first out / exit) | 1 / 1 | 1 / 1 | 1 / 1 | 1 / 1 |
| header_elements_cloned (first out / exit) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

#### Stages: first call of one large routine

| median ms | ubig n=0 b8 m0 | one n=0 b8 m0 | ubig n=1024 b8 m0 | one n=1024 b8 m0 | ubig n=4096 b8 m0 | one n=4096 b8 m0 |
|---|---:|---:|---:|---:|---:|---:|
| lex | 0.20 | 0.21 | 1.15 | 1.08 | 3.75 | 3.75 |
| parse | 3.74 | 3.73 | 9.11 | 9.21 | 25.19 | 25.81 |
| freeze | 0.31 | 0.31 | 3.31 | 3.15 | 11.22 | 11.36 |
| publish | 0.02 | 0.02 | 0.19 | 0.18 | 0.69 | 0.70 |
| materialize | 0.30 | 0.31 | 2.54 | 2.45 | 9.57 | 9.67 |
| resolve | 3.15 | 3.38 | 13.88 | 13.49 | 60.26 | 59.97 |
|   core_package_parse | 1.32 | 1.36 | 1.31 | 1.30 | 1.38 | 1.35 |
| specialize | 1.24 | 1.35 | 9.33 | 9.15 | 33.31 | 33.77 |
| const_eval | 0.67 | 0.70 | 5.85 | 5.60 | 21.73 | 21.69 |
| module_analysis | 6.08 | 6.46 | 46.16 | 44.74 | 160.08 | 158.95 |
|   module_declarations | 0.33 | 0.37 | 2.55 | 2.42 | 10.21 | 10.19 |
|   module_keys | 5.14 | 5.40 | 39.48 | 38.20 | 135.48 | 134.64 |
|   module_body_wave | 0.23 | 0.27 | 1.99 | 1.93 | 6.45 | 6.43 |
|     deferred_setup | 0.18 | 0.20 | 1.82 | 1.74 | 5.96 | 5.96 |
|     entry_lowering | 0.04 | 0.06 | 0.15 | 0.15 | 0.36 | 0.35 |
|   requires_tracing | 0.24 | 0.25 | 1.64 | 1.58 | 5.65 | 5.63 |
|   module_link | 0.24 | 0.25 | 1.73 | 1.67 | 5.94 | 5.92 |
| bytecode_module | 0.00 | 0.01 | 0.09 | 0.06 | 0.41 | 0.41 |
| body_analysis | 0.00 | 13.78 | 0.00 | 13.86 | 0.00 | 14.11 |
|   body_lower | 0.00 | 12.24 | 0.00 | 12.30 | 0.00 | 12.44 |
|   header_clone | 0.00 | 0.00 | 0.00 | 0.04 | 0.00 | 0.19 |
|   body_effects | 0.00 | 0.05 | 0.00 | 0.05 | 0.00 | 0.08 |
|   body_finish | 0.00 | 1.46 | 0.00 | 1.45 | 0.00 | 1.26 |
| bytecode_translate | 0.00 | 0.04 | 0.00 | 0.04 | 0.00 | 0.04 |
| time to first output | 13.43 | 14.48 | 90.77 | 87.31 | 329.21 | 327.97 |
| time to exit | 13.46 | 28.85 | 90.82 | 102.11 | 329.28 | 342.79 |
| peak live MB | 8.3 | 14.4 | 60.7 | 60.5 | 219.8 | 219.0 |
| allocated MB (total) | 34.5 | 82.4 | 256.4 | 307.3 | 928.9 | 986.2 |
| bodies_parsed (first out / exit) | 63 / 63 | 63 / 63 | 1087 / 1087 | 1087 / 1087 | 4159 / 4159 | 4159 / 4159 |
| bodies_frozen (first out / exit) | 1 / 1 | 1 / 1 | 1025 / 1025 | 1025 / 1025 | 4097 / 4097 | 4097 / 4097 |
| bodies_materialized (first out / exit) | 1 / 1 | 1 / 1 | 1025 / 1025 | 1025 / 1025 | 4097 / 4097 | 4097 / 4097 |
| bodies_resolved (first out / exit) | 3 / 3 | 3 / 3 | 1027 / 1027 | 1027 / 1027 | 4099 / 4099 | 4099 / 4099 |
| bodies_deferred_copied (first out / exit) | 3 / 3 | 3 / 3 | 1027 / 1027 | 1027 / 1027 | 4099 / 4099 | 4099 / 4099 |
| bodies_tracing_copied (first out / exit) | 3 / 3 | 3 / 3 | 1027 / 1027 | 1027 / 1027 | 4099 / 4099 | 4099 / 4099 |
| bodies_checked (first out / exit) | 0 / 0 | 0 / 1 | 0 / 0 | 0 / 1 | 0 / 0 | 0 / 1 |
| bodies_translated (first out / exit) | 1 / 1 | 1 / 2 | 1 / 1 | 1 / 2 | 1 / 1 | 1 / 2 |
| header_elements_cloned (first out / exit) | 0 / 0 | 0 / 3 | 0 / 0 | 0 / 1027 | 0 / 0 | 0 / 4099 |

#### Stages: call every routine

| median ms | unu n=256 b8 m0 | all n=256 b8 m0 | unu n=1024 b8 m0 | all n=1024 b8 m0 | unu n=4096 b8 m0 | all n=4096 b8 m0 |
|---|---:|---:|---:|---:|---:|---:|
| lex | 0.34 | 0.35 | 1.07 | 1.08 | 3.71 | 4.04 |
| parse | 4.15 | 4.13 | 7.88 | 8.92 | 25.28 | 27.49 |
| freeze | 0.74 | 0.77 | 2.83 | 3.19 | 11.03 | 12.27 |
| publish | 0.06 | 0.05 | 0.20 | 0.18 | 0.78 | 0.75 |
| materialize | 0.58 | 0.60 | 2.21 | 2.38 | 9.44 | 10.04 |
| resolve | 4.57 | 4.52 | 12.60 | 13.14 | 60.81 | 64.16 |
|   core_package_parse | 1.49 | 1.31 | 1.25 | 1.45 | 1.38 | 1.33 |
| specialize | 2.14 | 2.15 | 8.32 | 8.21 | 33.18 | 36.17 |
| const_eval | 1.37 | 1.30 | 5.32 | 5.44 | 21.86 | 22.88 |
| module_analysis | 10.68 | 11.16 | 40.23 | 44.84 | 158.16 | 172.19 |
|   module_declarations | 0.65 | 0.62 | 2.43 | 2.48 | 10.14 | 10.98 |
|   module_keys | 8.95 | 9.38 | 33.85 | 37.72 | 133.75 | 145.24 |
|   module_body_wave | 0.49 | 0.47 | 1.66 | 2.09 | 6.52 | 7.06 |
|     deferred_setup | 0.39 | 0.39 | 1.46 | 1.89 | 6.01 | 6.57 |
|     entry_lowering | 0.09 | 0.07 | 0.17 | 0.14 | 0.37 | 0.36 |
|   requires_tracing | 0.34 | 0.37 | 1.66 | 1.84 | 5.63 | 6.06 |
|   module_link | 0.37 | 0.39 | 1.75 | 1.95 | 5.94 | 6.36 |
| bytecode_module | 0.02 | 0.02 | 0.06 | 0.08 | 0.43 | 0.45 |
| body_analysis | 0.00 | 26.02 | 0.00 | 183.05 | 0.00 | 1709.91 |
|   body_lower | 0.00 | 19.85 | 0.00 | 89.68 | 0.00 | 355.01 |
|   header_clone | 0.00 | 2.58 | 0.00 | 50.44 | 0.00 | 768.91 |
|   body_effects | 0.00 | 0.63 | 0.00 | 9.18 | 0.00 | 128.05 |
|   body_finish | 0.00 | 1.10 | 0.00 | 5.42 | 0.00 | 21.99 |
| bytecode_translate | 0.00 | 0.10 | 0.00 | 0.40 | 0.00 | 1.99 |
| time to first output | 22.61 | 23.07 | 80.41 | 86.09 | 328.55 | 352.72 |
| time to exit | 22.66 | 52.25 | 80.46 | 308.48 | 328.62 | 2599.76 |
| peak live MB | 14.1 | 15.4 | 53.2 | 126.4 | 209.9 | 1695.4 |
| allocated MB (total) | 59.8 | 235.6 | 226.9 | 2358.2 | 895.8 | 30815.2 |
| bodies_parsed (first out / exit) | 318 / 318 | 319 / 319 | 1086 / 1086 | 1087 / 1087 | 4158 / 4158 | 4159 / 4159 |
| bodies_frozen (first out / exit) | 256 / 256 | 257 / 257 | 1024 / 1024 | 1025 / 1025 | 4096 / 4096 | 4097 / 4097 |
| bodies_materialized (first out / exit) | 256 / 256 | 257 / 257 | 1024 / 1024 | 1025 / 1025 | 4096 / 4096 | 4097 / 4097 |
| bodies_resolved (first out / exit) | 258 / 258 | 259 / 259 | 1026 / 1026 | 1027 / 1027 | 4098 / 4098 | 4099 / 4099 |
| bodies_deferred_copied (first out / exit) | 258 / 258 | 259 / 259 | 1026 / 1026 | 1027 / 1027 | 4098 / 4098 | 4099 / 4099 |
| bodies_tracing_copied (first out / exit) | 258 / 258 | 259 / 259 | 1026 / 1026 | 1027 / 1027 | 4098 / 4098 | 4099 / 4099 |
| bodies_checked (first out / exit) | 0 / 0 | 0 / 257 | 0 / 0 | 0 / 1025 | 0 / 0 | 0 / 4097 |
| bodies_translated (first out / exit) | 1 / 1 | 1 / 258 | 1 / 1 | 1 / 1026 | 1 / 1 | 1 / 4098 |
| header_elements_cloned (first out / exit) | 0 / 0 | 0 / 66563 | 0 / 0 | 0 / 1052675 | 0 / 0 | 0 / 16793603 |

#### Time to first output

Host, revision and cache state are in the JSON `meta`.

| sweep | jobs | workload | first out med | first out p95 | Δ vs n=0 (paired) | after-output med | after-output p95 | exit med |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| unused-routine count | 1 | unu n=0 b8 m0 | 7.0 | 7.6 | +0.0 | 0.5 | 0.7 | 7.5 |
| unused-routine count | 1 | unu n=256 b8 m0 | 25.1 | 29.6 | +18.0 | 1.2 | 1.5 | 26.3 |
| unused-routine count | 1 | unu n=1024 b8 m0 | 82.0 | 93.2 | +75.3 | 2.3 | 2.6 | 84.3 |
| unused-routine count | 1 | unu n=4096 b8 m0 | 326.5 | 338.8 | +319.4 | 6.5 | 6.9 | 333.2 |
| unused-routine count | 1 | unu n=16384 b8 m0 | 1528.3 | 1584.9 | +1521.4 | 23.4 | 24.9 | 1552.1 |
| body size (statements), n=1024 | 1 | unu n=1024 b2 m0 | 35.5 | 37.6 | +28.5 | 1.6 | 1.9 | 37.1 |
| body size (statements), n=1024 | 1 | unu n=1024 b8 m0 | 82.0 | 93.2 | +75.3 | 2.3 | 2.6 | 84.3 |
| body size (statements), n=1024 | 1 | unu n=1024 b32 m0 | 257.3 | 290.7 | +250.4 | 5.1 | 6.1 | 262.3 |
| body size (statements), n=1024 | 1 | unu n=1024 b128 m0 | 970.1 | 999.5 | +962.9 | 15.5 | 17.6 | 984.7 |
| import count (files), n=1024 | 1 | unu n=1024 b8 m0 | 82.0 | 93.2 | +75.3 | 2.3 | 2.6 | 84.3 |
| import count (files), n=1024 | 1 | unu n=1024 b8 m4 | 79.1 | 135.4 | +72.3 | 2.2 | 4.5 | 81.3 |
| import count (files), n=1024 | 1 | unu n=1024 b8 m16 | 80.8 | 90.1 | +73.6 | 2.2 | 3.0 | 83.0 |
| import count (files), n=1024 | 1 | unu n=1024 b8 m64 | 82.1 | 129.0 | +74.9 | 2.2 | 4.1 | 84.2 |
| first call of one large routine | 1 | ubig n=0 b8 m0 | 17.5 | 21.2 | +10.4 | 0.9 | 2.1 | 18.6 |
| first call of one large routine | 1 | one n=0 b8 m0 | 17.6 | 23.6 | +10.6 | 744.1 | 856.5 | 761.7 |
| first call of one large routine | 1 | ubig n=1024 b8 m0 | 93.4 | 99.2 | +86.3 | 2.5 | 2.7 | 96.1 |
| first call of one large routine | 1 | one n=1024 b8 m0 | 92.5 | 96.4 | +85.9 | 739.7 | 913.8 | 833.6 |
| first call of one large routine | 1 | ubig n=4096 b8 m0 | 341.0 | 361.3 | +333.8 | 6.9 | 7.7 | 348.5 |
| first call of one large routine | 1 | one n=4096 b8 m0 | 340.8 | 461.2 | +334.1 | 738.1 | 927.6 | 1078.7 |
| call every routine | 1 | unu n=256 b8 m0 | 25.1 | 29.6 | +18.0 | 1.2 | 1.5 | 26.3 |
| call every routine | 1 | all n=256 b8 m0 | 26.9 | 29.3 | +20.2 | 30.4 | 31.9 | 57.3 |
| call every routine | 1 | unu n=1024 b8 m0 | 82.0 | 93.2 | +75.3 | 2.3 | 2.6 | 84.3 |
| call every routine | 1 | all n=1024 b8 m0 | 89.1 | 96.2 | +81.9 | 503.2 | 636.6 | 591.1 |
| call every routine | 1 | unu n=4096 b8 m0 | 326.5 | 338.8 | +319.4 | 6.5 | 6.9 | 333.2 |
| call every routine | 1 | all n=4096 b8 m0 | 360.5 | 377.5 | +353.4 | 2564.0 | 5210.3 | 2922.2 |
| unused-routine count | 0 | unu n=0 b8 m0 | 7.3 | 10.1 | +0.0 | 0.6 | 0.9 | 7.9 |
| unused-routine count | 0 | unu n=256 b8 m0 | 23.8 | 25.6 | +16.5 | 1.3 | 1.5 | 25.1 |
| unused-routine count | 0 | unu n=1024 b8 m0 | 75.7 | 94.0 | +68.4 | 3.1 | 3.7 | 78.9 |
| unused-routine count | 0 | unu n=4096 b8 m0 | 292.0 | 302.7 | +284.7 | 9.6 | 11.1 | 301.5 |
| unused-routine count | 0 | unu n=16384 b8 m0 | 1382.7 | 1529.4 | +1375.8 | 37.1 | 40.4 | 1421.6 |
| body size (statements), n=1024 | 0 | unu n=1024 b2 m0 | 33.6 | 34.7 | +26.2 | 2.1 | 2.4 | 35.8 |
| body size (statements), n=1024 | 0 | unu n=1024 b8 m0 | 75.7 | 94.0 | +68.4 | 3.1 | 3.7 | 78.9 |
| body size (statements), n=1024 | 0 | unu n=1024 b32 m0 | 230.6 | 252.5 | +223.7 | 7.1 | 10.1 | 237.6 |
| body size (statements), n=1024 | 0 | unu n=1024 b128 m0 | 863.8 | 981.7 | +856.3 | 25.7 | 31.6 | 890.0 |
| import count (files), n=1024 | 0 | unu n=1024 b8 m0 | 75.7 | 94.0 | +68.4 | 3.1 | 3.7 | 78.9 |
| import count (files), n=1024 | 0 | unu n=1024 b8 m4 | 72.2 | 97.3 | +65.3 | 2.9 | 3.7 | 75.2 |
| import count (files), n=1024 | 0 | unu n=1024 b8 m16 | 75.3 | 78.9 | +67.8 | 2.8 | 3.2 | 78.2 |
| import count (files), n=1024 | 0 | unu n=1024 b8 m64 | 80.4 | 84.0 | +73.3 | 3.0 | 3.2 | 83.1 |
| first call of one large routine | 0 | ubig n=0 b8 m0 | 17.8 | 21.3 | +10.7 | 0.9 | 1.1 | 18.9 |
| first call of one large routine | 0 | one n=0 b8 m0 | 17.5 | 19.2 | +10.1 | 741.0 | 799.9 | 758.9 |
| first call of one large routine | 0 | ubig n=1024 b8 m0 | 86.1 | 91.7 | +78.5 | 3.2 | 3.8 | 89.1 |
| first call of one large routine | 0 | one n=1024 b8 m0 | 85.8 | 92.5 | +78.3 | 739.6 | 782.1 | 825.7 |
| first call of one large routine | 0 | ubig n=4096 b8 m0 | 307.2 | 341.0 | +299.9 | 9.9 | 10.8 | 317.6 |
| first call of one large routine | 0 | one n=4096 b8 m0 | 307.0 | 316.9 | +298.5 | 740.9 | 786.4 | 1053.3 |
| call every routine | 0 | unu n=256 b8 m0 | 23.8 | 25.6 | +16.5 | 1.3 | 1.5 | 25.1 |
| call every routine | 0 | all n=256 b8 m0 | 25.7 | 27.4 | +18.3 | 30.9 | 32.3 | 56.5 |
| call every routine | 0 | unu n=1024 b8 m0 | 75.7 | 94.0 | +68.4 | 3.1 | 3.7 | 78.9 |
| call every routine | 0 | all n=1024 b8 m0 | 82.3 | 124.6 | +75.4 | 500.5 | 852.8 | 582.7 |
| call every routine | 0 | unu n=4096 b8 m0 | 292.0 | 302.7 | +284.7 | 9.6 | 11.1 | 301.5 |
| call every routine | 0 | all n=4096 b8 m0 | 325.3 | 375.1 | +318.1 | 2571.3 | 4304.9 | 2892.5 |
