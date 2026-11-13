# Cyallo interpreter and tiered compiler

A typed language with a compact register-bytecode interpreter, background JIT compilation, a persistent REPL, Oxc diagnostics, syntax colors, and a Ratatui debugger. **Modern source uses `.ya`; CAIE compatibility source uses `.psc`.** File commands select the dialect from the extension. The REPL and files with other extensions default to modern syntax; `--dialect` overrides the selection.

The workspace uses `cyallo` for the runtime and CLI, `cyallo-parser` for modern syntax,
`cyallo-ast` for the shared source model, `cyallo-ir` for resolution, type analysis and
typed IR, `cyallo-values` for runtime values, and `cyallo-compile-jobs` for compilation
scheduling. `caie-int-parser` parses CAIE compatibility syntax into the shared AST.
The runtime separates C/T/P execution (`cyallo-scheduler`),
MPMC channels (`cyallo-channel`), operations (`cyallo-io`), timers (`cyallo-clock`),
and blocking work (`cyallo-host`). See [concurrency](docs/concurrency.md) for the
APIs, instance ownership, and remaining compiler/VM integration.
The [Cyallo concurrency examples](examples/concurrency/README.md) demonstrate
language-level generators. Task, channel, and managed transport integration is
still in progress.

```sh
cargo run -- run examples/tour.ya
cargo run -- run examples/osr-loop.ya --jit-report
cargo run -- repl
cargo run -- pretty examples/modern.ya --color always
cargo run -- debug examples/modern.ya
cargo run -- check examples/loops.psc
cargo run -- bytecode examples/modern.ya
cargo run -- bytecode examples/modern.ya --wasm 1
```

Start with the [example guide](examples/README.md) for a reading order, run commands,
and expected output, including generics, closures, traits, locations, and user-defined
service lookup.

## Modern syntax

```text
use std::{array, io, num, string}

fn square(n: i64) -> i64 { return n * n }
let mut total = 0
let names = ["  Ada  ", "Grace", "Linus"]
for i = 1 to 4 { total += square(i) }
io::print(string::upper(string::trim(names[0])))
io::print(total, ":", array::sum(array::from(1..5)), ":", names.len())
```

Names and imports are case sensitive. `let` introduces an immutable binding;
`let mut` permits reassignment and mutable borrowing. `const` evaluates at compile
time. Blocks introduce lexical scopes. Assignment requires an existing binding.

Integers use `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `isize`, or
`usize`; floats use `f32` or `f64`. Unsuffixed literals take their context's numeric
type, defaulting to `i64` and `f64`. Established numeric types require explicit,
checked `as` casts. Integer `/` produces `f64`; `div` truncates toward zero.

| Type or expression | Meaning |
| --- | --- |
| `[N]T` | Owned fixed array |
| `[]T` | Unsized array type |
| `&[]T`, `&mut []T` | Borrowed slice with data and length |
| `&[N]T` | Reference to a fixed array |
| `[1, 2, 3]` | Fixed array literal |
| `std::collections::Vec[1, 2, 3]` | Growable vector |
| `std::collections::Vec<u8>[]` | Empty vector with an explicit element type |
| `std::collections::VecDeque[1, 2]` | Growable deque backed by a ring buffer |
| `std::array::from(0..4)` | Fixed array with statically known length |
| `std::collections::Vec::from(0..n)` | Vector from runtime range bounds |
| `std::array::empty<T>()` | Fixed array of type `[0]T` |

Aggregates move by default. `std::mem::clone(&value)` explicitly clones an owned
value. References are explicit: `&value` or `&mut value`; references can be passed
to routines and used as receivers. Local aliases are permitted, while opaque call
boundaries enforce aliasing XOR mutability. Live collection views prevent resizing.

`fn` is pure: private local mutation and ordinary value allocation are permitted,
but I/O, mutable external reads, and effectful lifecycle ownership are rejected.
`proc` may perform effects and may return a value. A pure function may borrow an
RC value through an effectless reference; it cannot own, construct, clone, or drop
one, including through a record or capture.

## Standard library and imports

Standard functions are ordinary resolved declarations with intrinsic identities.
A function's spelling does not select a compiler opcode. The semantic checker
resolves its declaration and types; IR carries the resulting typed operation.
Shadowed functions and indirect calls keep their own callable identities.

| Namespace | Examples |
| --- | --- |
| `std::array` | `from`, `empty`, `len`, `sum` |
| `std::collections` | `Vec`, `VecDeque` |
| `std::num` | `parse<T>`, `abs`, `min`, `max`, `clamp`, `sqrt`, `pow` |
| `std::string` | `from`, `trim`, `upper`, `lower`, `contains`, `replace` |
| `std::char`, `std::random` | `code`, `from_code`, `uniform` |
| `std::cell` | Owned `Cell<T>` and `Cell::new` |
| `std::gc`, `std::rc`, `std::mem` | Explicit managed ownership, weak refs, cloning and dropping |
| `std::lookup` | Retained field `Location<T>`, relative `StoragePath<T>`, `mapped` |
| `std::option`, `std::result`, `std::any` | Optional values, fallible results, checked erasure |
| `std::io` | `print` |
| `std::service` | Typed service contexts, keys, registrations and references |
| `std::runtime::caie` | Compatibility operations, including `std::runtime::caie::input(&mut value)` |

`std::num::parse<u32>(text)` returns `Result<u32, std::num::ParseError>`.
Use `is_ok`, `is_err`, `unwrap`, or `unwrap_err` to handle it.

`std::prelude` implicitly reexports module names such as `array`, `num`, `gc`,
`io`, and `mem`, plus common types including `Cell`, `Vec`, `VecDeque`, `Option`,
and `Result`. Individual utility functions require a qualified path or `use`.

```text
mod math;                         // math.ya or math/mod.ya
mod detail { pub const SCALE = 2 }
use crate::math::{square, cube as third_power}
use std::num::{abs as magnitude, max}
pub use crate::detail::*;
```

Modules support inline declarations, external files, grouped imports, aliases,
globs, and reexports. `crate`, `self`, and `super` anchor relative paths. `pub`
exports declarations. Only declared modules are loaded; module loading does not
activate service providers. See [modules and services](docs/services.md).

## Managed values and storage

`@gc T` and `@rc T` are dedicated types. `@gc expression` allocates a traced shared
value; `@rc expression` creates a session-local reference-counted owner. Explicit
managed syntax enables the necessary collector integration without `--gc`.
`std::rc::clone(&owner)` increments the strong count; `downgrade`, `upgrade`,
`strong_count`, and `weak_count` use resolved RC declarations. Strong cycles are
not collected by reference counting.

`Cell<T>` owns its contents. `@gc Cell<T>` expresses shared interior-mutable
storage explicitly. `Vec<@gc Cell<T>>` composes growable storage with shared cells.
`*cell` copies the contents and requires Copy; `*cell = value` replaces them.
`cell.replace(value)` returns the previous owned value. Cell contents expose no
references and do not support compound assignment. Recursive records may use
managed or collection indirection; unreachable GC cycles are collected.
`std::lookup::field(&record_owner, "field")` creates a strong retained field
location. A `StoragePath<T>` is a relative recipe, and `std::lookup::mapped`
returns the current `Option<T>` after validating its cached dependencies.
See [managed ownership and allocators](docs/managed-memory.md),
[the lookup reference](docs/lookups.md), and [examples](examples/README.md).

## Bytecode and JIT tiers

```text
Source → AST → declarations → per-body checking/inference → typed control-flow IR
       → register bytecode → interpreter / native Winch / Cranelift
```

Scalar/control instructions are 16-byte words with explicit register IDs, branch PCs, and type tags. Source spans and variable-sized operations live separately. `bytecode <file>` disassembles them; `bytecode <file> --wasm <function-id>` displays the optional legacy Wasm adapter. The native baseline consumes bytecode directly. The adapter requires matching runtime metadata and imports; it is not a standalone WASI application or a stable persisted package format.

Default execution uses `--engine jit`. A dedicated **`cyallo-codegen-N` worker pool** performs native compilation. The default worker count is `min(3, logical CPUs - 1)`, with one worker on single-core/unknown hosts. A Kanal-backed priority scheduler coalesces requests and retains the highest desired tier even while another job is queued or running. Hot upgrades take priority; aging gives older requests a turn. Code is installed on the execution thread at safepoints.

The automatic JIT can proactively compile likely repeated work in the **[16, 128) bytecode-word window**. It uses syntax-only estimates, direct-call lookahead and recent compilation costs, then checks actual lowered size on a worker. It permits four outstanding eager jobs, 256 estimated speculative words and 2 ms of predicted compilation work. First entries share **one 1 ms wait budget per artifact**; a timeout continues interpretation. `TierPolicy::eager`, `first_call_wait`, and `TieredArtifact::hint_call_soon(function_id)` expose the policy. `CAIE_EAGER_ROUTINES=0` disables prediction for comparison.

`run` with `jit` or `interpreter` checks declarations and the entry body up front. Other bodies are checked/lowered once on use or selection by the proactive JIT; unrelated cold bodies retain syntax without detailed analysis. A deferred semantic error is raised at its original span before that body executes. `check`, the `bytecode` command, debugging, explicit eager Cranelift engines, and the REPL fully validate bodies. Library `compile()` remains eager; use `compile_for_execution(source, options)` for deferred analysis. Dump flags do not force cold bodies to be analyzed.

Modern functions and procedures accept independent routine attributes:

```text
#[hint(hot)]       // auto | hot | cold: automatic call/work thresholds
#[opt(eager)]      // auto | eager | lazy | always_eager | always_lazy
#[inline]          // auto | preferred | never | always; bare inline = preferred
fn inc(n: i64) -> i64 { return n + 1 }
```

`opt(lazy)` suppresses predictive JIT jobs while allowing normal observed-work tiering and inlining. `opt(always_lazy)` forbids all native compilation and inlining, including explicit eager engines. `always_eager` bypasses the automatic size window, retaining hard limits. `inline(always)` can retain real frames for supported branching functions and procedures; it never bypasses correctness limits. Unfulfilled requests are silent unless `--warn-unfulfilled-attributes` is set. See [policy, limits and measurements](docs/jit-inspection-and-admission.md).

`--emit ir,bytecode,clif --emit-dir ./compiler-output` writes typed IR, register bytecode, and generated Cranelift IR before/after optimization. A manifest identifies deferred routines. `--jit-notices` prints bounded inline, optimization, eager-admission, fallback and invalidation notices at execution safepoints. These options preserve ordinary inlining; `--jit-trace` remains the intrusive tracing mode.

Independent parsing, checking, bytecode lowering and dump formatting use bounded jobs with ordered publication. Eager native compilation uses up to three workers; background compilation also honors the artifact's worker allowance. `--jobs 1` selects serial compilation within an artifact. Execution and code installation preserve source order on the Session thread.

Small routines are no longer compiled merely because they are small. Automatic
tiering keeps a routine interpreted only briefly, then installs Winch, which
compiles in well under a millisecond. `TierPolicy` controls the Winch call and
work thresholds, the Cranelift warm threshold, the speed threshold, and the
body-size cap for speed codegen.

| Routine | Automatic tier |
| --- | --- |
| Fewer than 8 calls and under 2,000 executed instructions | Interpreted |
| 8 calls, or 2,000 executed instructions | Winch (cheap baseline, sub-millisecond compile) |
| At least 5,000 executed instructions | Cranelift fast (`opt_level=none`, backtracking allocation) |
| Loop-only body, at least 8,000,000 executed instructions, at most 512 bytecodes | Cranelift speed (`opt_level=speed`, backtracking allocation) |

Body shape is recorded without lowering a deferred body: loops and calls are
known from syntax/bytecode metadata. Speed codegen is intentionally reserved for
hot loop bodies without calls and is skipped for large bodies because its
compile time grows sharply with body size.

Native routines notify the driver at their next unrequested promotion threshold,
including Winch-to-fast for loops and repeatedly called leaves. A pending or
failed request does not repeatedly trigger the same yield. Compilation remains
asynchronous; crossing a threshold requests code, while installation waits for
worker completion. Readiness polling uses a separate configurable interval.

Tiering is monotonic. A hot loop can promote **during the same function invocation**. Interpreter, Winch, and Cranelift fast resume at every bytecode PC; automatic speed units enter at function starts and loop headers. A speed invocation paused elsewhere resumes through a speed-compiled continuation until its next optimized entry. Native loops poll readiness and promotion thresholds without returning on every execution quantum; instruction budgets remain exact. Locals, virtual registers, BYREF aliases, return destinations, and prior side effects remain in the shared VM frames. This is VM-frame OSR, not reconstruction/deoptimization of a native machine stack. The OSR tests verify both a single long invocation and aliased caller storage.

`--jit-report` prints a compact summary while preserving normal optimization and inlining. It shows logical instructions/calls (including inlined routines), inline counts, GC/service counters and a bounded compiler timeline. It does not enable per-call clocks or create physical frames for inlined bodies. Its VM execution time includes deferred checks, compiler admission waits and GC; run wall excludes frontend, process startup and report rendering.

`--jit-trace` enables the detailed invocation/tier timeline, per-function timing, runtime boundary counters and VM-frame OSR observations. This intrusive mode disables inlining and adds observer overhead. Use it to inspect execution transitions; use the normal run or summary mode to measure optimized performance.

```sh
cargo build --release
./target/release/cyallo run examples/osr-loop.ya --engine jit --jit-report
./target/release/cyallo run examples/hot-loop.ya --engine cranelift_speed --jit-report
./target/release/cyallo run examples/osr-loop.ya --engine jit --jit-trace
```

The full trace separates frontend/read, deferred body checks, backend/session setup, VM execution, first-call compiler wait, input wait, GC pauses, and run wall time. Its VM execution headline excludes separately reported body checks, compiler wait and GC pauses; observed per-function phases can include helper-driven analysis and collection. Per-tier instruction totals/shares include runtime helpers in their calling tier. Trace function summaries show invocations, instructions, OSR counts and execution time; peak frame/cell counts describe physical VM storage. Compiler work overlaps execution and must not be added to VM time. Trace events are capped at 4,096; summary compiler events are capped at 64. Omitted-event counts and complete logical counters remain available.

Tiny programs can finish before compilation completes; the report shows pending compilation and does not wait. Reports also work with the interpreter, both explicit Cranelift engines, runtime failures, and each accepted REPL entry. Unsupported Winch compilation falls back to Cranelift fast and records why. Dropped artifacts cancel queued, not-yet-started jobs. Active compilation is allowed to finish safely.

Cranelift inlines bounded direct scalar calls by default: small integer/boolean functions and procedures can run without VM frames. Small effectful procedures can share embedded code while retaining real frames and helper/GC boundaries; this path is opt-in through `CAIE_INLINE_PROCEDURES=1` or `CraneliftConfig.procedure_inlining`. Logical instructions and calls still count toward fuel and promotion thresholds. Full execution reporting disables both paths. Dynamic calls remain out of line, and native artifacts reject a replacement Program. Set `CAIE_INLINE_ROUTINES=0` or `CraneliftConfig.inlining = false` to disable all inlining; see [routine optimization results](docs/routine-optimization-results.md).

`--engine interpreter` disables native compilation. Both explicit Cranelift engines synchronously compile every routine with all-PC resume entries and link their native entry slots: `--engine cranelift` defaults to `opt_level=none` with backtracking (`Ion`) allocation; `--engine cranelift_speed` uses `opt_level=speed` with the same allocator. They share dispatch and resume structure, and neither builds selective-entry continuations or a large combined compiler graph. Speed optimization can increase compilation latency; it does not promise lower total latency on every short run. Effective configurations appear in `--jit-report`. Library embedders can use `CraneliftConfig` to vary optimization, combined/per-routine scope, resume strategy, and allocator independently. The default `--engine jit` retains automatic tiering with replaceable per-routine units. The public `TierPolicy` controls warm/hot thresholds, the speed body-size cap, and baseline admission.

```sh
cyallo run program.ya --engine cranelift
cyallo run program.ya --engine cranelift_speed
```

Native Winch and Cranelift read and write scalar registers and cells directly through a `repr(C)` VM view. Checked arithmetic, initialization checks, comparisons, and branches are native; PC and counters are batched at safepoints. Available native callees use stable entry slots across tiers. Scalar state is materialized before runtime calls, budget exits, and engine handoffs, preserving BYREF aliases and VM-frame OSR. Allocation, call setup/return, complex values, and I/O retain helpers; Winch uses native branch edges and a small write-through scalar register cache, with no Wasm/Wasmtime transitions in scalar loops. The old runtime adapter is available only with `--features wasm-adapter` for comparison. The CLI and benchmarks use [mimalloc](https://github.com/purpleprotocol/mimalloc_rust); library embedders choose their own allocator. See [architecture](docs/architecture.md) and [measurements](docs/benchmarks.md).

## CAIE compatibility and migration

Use `.psc` for the core [Cambridge 9618, 2027–2029 guide](https://www.cambridgeinternational.org/Images/721401-2027-2029-pseudocode-guide.pdf). Use `--dialect compatibility` to override another extension or select compatibility syntax in the REPL:

```sh
cyallo run exam.psc
cyallo repl --dialect compatibility
```

Compatibility mode requires DECLARE, preserves CAIE control structures and built-ins, and rejects modern syntax/utilities. `--convenience-builtins` explicitly enables utility functions there. `--dialect strict` remains an alias for the prior strict behavior; `--dialect simplified` preserves the prior declaration-free CAIE syntax. Library `compile()` now defaults to `Dialect::Modern`; use `compile_with_options` and `Dialect::Compatibility` for standard source.

Supported CAIE core: primitive types, literal constants, arrays with explicit signed bounds, assignment, console I/O, IF, CASE/ranges, WHILE, REPEAT, FOR/STEP, procedures/functions, recursion, BYVAL and BYREF, and the standard string/numeric built-ins. Files, DATE, records, enums, pointers, sets, and classes remain unsupported. LLVM and standalone native executables are deferred.

## REPL, colors, and debugger

The persistent line-oriented REPL accepts statements, bare expressions, and multiline blocks/routines. `:vars` inspects globals, `:reset` clears the session, `:cancel` discards an incomplete entry, `:quit` exits, and `:help` lists commands. Previous statements are not replayed. Static errors leave prior state unchanged; runtime errors preserve writes already performed. INPUT consumes a separate line. The per-entry instruction limit defaults to one million (`--max-instructions`). Definitions/types stay fixed; routine redefinition is not supported. Native caches are scoped to a validated program, rather than shared across REPL submissions.

`pretty <file>` uses the same lexer and preserves the original source. `--color auto|always|never` controls syntax colors and Oxc diagnostics. Auto detects stdout for pretty/prompts and stderr for diagnostics. `NO_COLOR` and `TERM=dumb` disable auto color; `always` overrides them.

The Ratatui debugger uses the interpreter. Keys: `c` continue, `p` pause, `s` into, `n` over, `o` out, arrows select a line, `b` toggle breakpoint, `r` restart, `q`/Esc quit. It shows source, stack, variables, sparse entries, and program I/O. During INPUT, type a value and press Enter; Esc exits.

## Runtime contract and verification

INTEGER is checked signed 64-bit; REAL is IEEE binary64 without fast math. `/` produces REAL, DIV truncates toward zero, MOD follows the dividend's sign, and INT truncates the fractional part. Evaluation is left to right; AND/OR and `&&`/`||` short-circuit. FOR bounds/step are evaluated once. CHAR and string positions count Unicode scalars. OUTPUT/print concatenate values and append a newline.

Uninitialized reads, holes, bounds errors, overflow, invalid input, and division by zero stop the current execution with Oxc source diagnostics. Tagged values live in `cyallo-values`; immutable strings use shared `Arc<str>` storage. Known types drive native scalar lowering; NaN boxing is not used. Default limits are one million occupied cells and 1,024 frames. Instruction budgets do not bound time spent inside one library operation.

`--gc` enables the experimental precise moving/compacting cell collector for run,
REPL, and debugger sessions. `--gc-stress` implies `--gc` and collects before
every cell allocation. Explicit roots preserve aliases, suspended frames, and
same-tier native continuations; there is no machine-stack scanning. With GC off,
returned routine-local cells are recycled for later calls, while other unreachable
cells and arena capacity remain reserved until restart/drop. With GC on, unreachable
cells and their owned buffers are reclaimed, while named globals/locals remain inspectable.
The planned 64 MiB total byte cap and native x64 validation remain rollout gates;
current limits count cells rather than all bytes. See [GC progress](docs/gc-plan.md)
and [measurements](docs/gc-benchmarks.md).

```sh
cyallo run examples/gc-cells.ya --gc --jit-report
cyallo repl --gc-stress
cargo bench --bench gc
```

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
cargo xtask vendor-winch check
cargo bench --bench engines --features wasm-adapter
```

Tests cover engine parity, all three native tiers, worker-thread compilation, OSR, aliases, typed bytecode, syntax/mode isolation, sparse arrays, utilities, REPL recovery, terminal output, lexer allocations, and UTF-8 spans. Benchmarks separate prewarming from median execution time; compilation latency and warm runtime speed are different measurements.

Vendored Winch provenance, exact patch reconstruction, isolated candidate updates, and the weekly upstream watch are documented in [vendor maintenance](vendor/README.md). Local execution validation covers ARM64 macOS; native Linux ARM64/x64 CI gates are configured and have not been run from this workspace. Windows x64 emission remains experimental.
