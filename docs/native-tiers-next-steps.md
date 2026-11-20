# Finishing the native tiers: instructions for the next sessions

Native slices 1, 2a, 2b, 3a, 3b, 3c and 4.1 are implemented. The shared runtime
also covers closures and calls, managed memory, input, module variables and randomness
from 4.3–4.5. Goal from the
project owner: **any code must be able to run in the native tiers**; the interpreter
stays the reference and the fallback. Native work does not block the legacy removal
([legacy-removal-plan.md](legacy-removal-plan.md)). Read
[compiler-and-runtime-rules.md](compiler-and-runtime-rules.md) first: typed errors,
declaration-based primitives, instance-owned runtime state.

## 1. How the native tier works today

Code map (all under `src/backend/verified/` unless noted):

| File | Role |
| --- | --- |
| `prepare.rs` | Legalizes verified bytecode into a `PreparedFunction`: `Operation`s per instruction (`Site`), the frame map (`FrameMap`), layout table (`Layout`), word table (`WordOp`), bridge table (`BridgeCall`), element accesses (`ElementAccess`), call table. **Anything not legalized returns `UnsupportedOperation`/`NonScalar` and the routine stays in the interpreter.** |
| `cranelift.rs`, `winch.rs` | Two adapters that emit machine code from the same plan. Every `Operation` needs both. Cranelift keeps frame words in SSA variables except *borrowed* words (see 3); Winch keeps every word in frame memory. |
| `run.rs` | The driver: runs one native function per invocation, handles calls (copying words between frames), returns, failures, bridges, and recovery into the interpreter (`NativeActivation::recover`). Owns the `Heap` (strings, array and vector buffers) and the output sink. |
| `run.rs::SharedActivation` | Keeps verified runtime frames and memory for a module that uses the shared runtime. Copies only initialized scalar homes across each native section and executes the requested runtime instruction at a bridge. |
| `abi.rs` | `ScalarOutcome` (Returned, Paused, Failed, Call, Bridge), `Failure` codes. |
| `publish.rs`, `validate.rs`, `image.rs` | Code store, linking, image checks. |
| `src/bytecode/verified/run.rs` | The interpreter (reference). Native recovery enters through `restore_scalars`, `RecoveredValue`, `RecoveredStep`. |

Execution model: a frame is an array of 64-bit words. Scalars take one word; tuples/
enums of scalars take consecutive words (`Layout`); a string is an index into the
activation's string table; arrays and vectors hold addresses of buffers owned by the
activation. A loan is an address with the low bit set for fixed-array elements.
A native function exits with
`Paused` (out of budget), `Call`, `Bridge`, `Failed` or `Returned`; the driver acts and
re-enters. Every exit leaves the frame in a state the interpreter can continue from:
Cranelift **spills** initialized homes before `Call`/`Bridge`/`Paused`; recovery maps are
per instruction (`Site::recover`).

## 2. The checklist for supporting one more instruction

1. **Read the interpreter arm** in `src/bytecode/verified/run.rs` (search
   `Instruction::Name`). Its semantics, failure variants and spans are the contract.
2. **Representation.** Does the value fit in words? If yes add a `Layout` variant (or reuse
   one) and give `layout_of`/`layout_of_top` and `FrameMap::layout_of_register` the case.
   If not, it needs a heap (see section 4).
3. **Legalize** in `legalize()` (`prepare.rs`) for word-based plans: produce an existing `Operation`, a
   word-move (`Operation::Words`), or a new `Operation`/`BridgeCall`. Remove the
   instruction from the rejection list at the bottom. Reject (`return Err(unsupported)`)
   every shape you do not handle, e.g. a destination that overlaps a source.
4. **Failures.** New failure kinds need a `Failure` code (`abi.rs`), the `failures()` table
   in `prepare.rs`, and the mapping to the interpreter's `RunError` in `run.rs`.
5. **Both adapters.** Cranelift (`cranelift.rs::operation`) and Winch
   (`winch.rs::operation`). Winch has few scratch registers (`LEFT RIGHT VALUE TEMP
   ADDRESS SAVED_DIVIDEND`); `address()`/`through()` use `ADDRESS` for large offsets, so
   do not hold values there across `load_home`/`store_home`.
6. **Recovery.** If a word-based plan produces a new kind of register value, extend
   `RecoveredValue` (interpreter side `recovered_value`, backend side `decode_layout`)
   and make sure the interpreter's expected storage state matches.
7. **Tests** in `tests/verified_native_*.rs`: run the same program on the interpreter and
   on `[Winch, Cranelift(LowLatency), Cranelift(Optimized)]`, compare result, output,
   executed-instruction count and failure kind/span, then pause at **every** budget
   (`1..instructions`), `recover()` and compare the finish. `tests/verified_native_bridges.rs`
   has the helpers (`check`, `check_module`, `publish_all`). Include failure cases.
8. Update the stage doc paragraph (`docs/overhaul-stages.md`) and the tests that assert
   "stays in the interpreter" (they must keep pointing at something genuinely unsupported).

## 3. Pitfalls already hit (do not repeat)

- **Address-taken words must live in memory in Cranelift.** `Borrow`/projection owners are
  recorded in `FrameMap::borrowed`; `load_at`/`store_at` and `spill` honor it. Any new
  instruction that takes the address of frame words must add them.
- **Loan words are tagged.** `ReadLoan`/`WriteLoan` mask bit 0; element loans carry it.
  A new loan-producing instruction must produce an address the same code can use.
  (The first Cranelift `ReadLoan` forgot the mask; Winch hid it.)
- **Sources that are loans or owners.** `EnumIs` accepts a loan; check the operand's
  layout (`enum_home`, `LoanBase`) before treating a home as the value.
- **Overlap.** Multi-word moves are sequential. Reject destination/source overlap.
- **Slot sharing.** Registers with disjoint lifetimes share verifier slots; `FrameMap`
  sizes a slot by its largest occupant. Never assume a slot belongs to one register.
- **Move vs copy of owned values.** Copying a word that is an owner (array address)
  aliases it. Copy of arrays is a bridge (`CopyArray`); call arguments are deep-copied in
  the driver; `Move` transfers. Check every path that copies words (Tuple, Extract,
  EnumWrap, call arguments and results, ReadLoan/WriteLoan) for owned kinds.
- **Release exactly once.** Release at `StorageDead` only when the static storage state is
  `Initialized`; `MaybeInitialized` is already rejected (`ConditionalInitialization`).
- **Side effects run once.** Bridges run in the driver after the exit; resumption is at the
  successor with `StatementArrival::Next(pc)`. Never perform an effect before the fuel
  check or re-run it on recovery.
- **Stale "interpreter-only" assertions** keep breaking when coverage grows; run the full
  verified suite (command in 6) after every slice.
- **Tooling quirks.** `rustfmt` reflows code, so scripted text replacement against
  formatted source silently misses; verify replacements applied. Commit signing through
  1Password fails intermittently; retry the commit with a short sleep, never bypass it.

## 4. Remaining work, in order

### 4.1 Scalar vectors and ranges (implemented)

Fixed arrays keep a written flag per element. Vectors store one word per initialized
scalar in a buffer headed by length and capacity. Native instructions handle length,
indexed access and element loans. Bridges handle creation, growth and deep copies.
Ranges reuse the inline pair of signed bounds from verified bytecode. Inclusive
range construction checks the end before incrementing it. Recovery restores vectors
and their loans, including reborrows that the bytecode verifier accepts across growth.
See the tests in [verified_native_bridges.rs](../tests/verified_native_bridges.rs).

Pack vector elements by type once sub-word layouts exist. Support composite elements
once their layout and copying rules are available. Whole-vector reads and writes
through loans still use the interpreter. Elements with drop semantics wait for 4.4.

### 4.2 Cells, slices, sums, extrema, remaining text (next)
- `CellNew/Read/Write/Replace` of scalar contents: a cell is its content inline
  (`Layout::Cell` = element layout); reads and writes are word moves; shared-loan writes
  work because loans are addresses.
- `SliceOf`, `SliceLength`, `Sum`, `Extremum`: a slice is a window (address, length,
  start) over an array or vector; keep it as a loan-like pair of words and decide how a
  `Slice` loan recovers (`Window` in the interpreter place).
- `TextTest`/`TextMap` already bridged; add the not-yet-covered text instructions and
  arrays of text only after 4.4 (they need element drop semantics).

### 4.3 Closures, indirect calls, host bindings, and methods (implemented)

Shared-runtime plans execute frame-local scalar instructions in Winch or Cranelift.
At a runtime bridge the driver spills initialized scalar homes, restores the
instruction and statement edge, and executes one verified instruction. The runtime
keeps closure captures and resolves dynamic calls. The native driver selects the
callee from the activation's snapshot of published entries. A callee without a
published entry runs in the interpreter, and the driver can re-enter native code at
the caller. Host callbacks and trait dispatch use the same verified call implementation.

### 4.4 Counted owners and GC handles (implemented)

The shared runtime retains the entire verified stack and heap. Managed values and
loans stay in their verified slots, so the collector traces suspended callers and
the current frame through its existing root scan. Allocation, clone, count, weak
upgrade and drop use the verified operations. Allocator-backed drops push finalizer
frames on the shared stack. Recovery returns that activation with the same heap,
counts and stack. Tests exercise every budget under normal collection and GC stress.

### 4.5 Input, module variables and randomness (implemented)

The shared runtime owns input, globals and random state. Waiting input preserves
the instruction and incoming statement edge without spending fuel. Closing the
source delivers the typed end-of-input failure. Globals retain their initialization
state, and recovery preserves the input source and random seed. Clock operations
belong with broader suspension support in 4.6.

### 4.6 Extend the shared runtime bridge

Extend native admission and coverage to objects, erased values, services, module loads,
generators, tasks, and channels. Use the same single-instruction bridge and preserve
the runtime's effect ordering, instruction counts and suspension across handoffs.
The shared runtime can already retain these values while it runs a bridge. Modules
that only use those features still need native admission and coverage. Verify
re-entry after module publication and scheduler changes, then add native park and
resume tests for the suspended frames described in stage 8 of
[overhaul-stages.md](overhaul-stages.md).

### 4.7 Caches (deferred until here, per the project owner)
Service slot and call-site caches, lookup-plan caches, object shape descriptors and inline
caches. They are optimizations over 4.6's semantics: build them against the interpreter
first (`overhaul-stages.md` stage 7 describes guards and snapshots), then add native
guards. Not needed for correctness.

### 4.8 Performance work after coverage
Inlining of direct native calls, loop OSR from interpreter to native
(`recover` is the reverse direction; OSR needs `import`), register-resident arrays
(promote small fixed arrays to frame words), removing per-bridge spills, tiering policy
(`Winch` first, `Cranelift` for hot code), and benchmarks recorded before/after
(storage, startup, throughput). Rebuild JIT reports and the trace viewer on this tier
(project owner: wanted, not urgent).

### 4.9 Native-ineligible leftovers to decide
`u64` counts have one-word homes and checked conversions. Count arithmetic and
unsigned comparison instructions remain open. Chars and 32-bit numbers need
verified kinds first (legacy-removal decisions 1-2). The simplified dialect needs
sparse arrays in the verified engine first.

## 5. Interpreter-side prerequisites to remember
- `Activation::restore_scalars` rebuilds native word frames in the interpreter and
  validates storage state and shapes. Shared-runtime plans retain the existing
  activation, so they only synchronize the scalar homes at section boundaries.
- Interpreter output goes through `OutputSink`; native shares the sink on recovery.
- The perf session owns `src/dynamic_modules.rs`, `crates/cyallo-ir/src/analysis/modules.rs`,
  `src/bytecode.rs`, `crates/cyallo-ir/src/{ir,analysis,osr}.rs`: message that session
  before touching them. `src/bytecode/verified*` and `src/backend/verified*` are free.

### Interpreter frame storage

Ignition and JavaScriptCore's native LLInt both put call headers and local slots
on a contiguous stack. On 64-bit targets each operand identifies a slot relative
to the frame pointer. In the inspected ARM64 Ignition layout, local zero is
56 bytes below the frame pointer and the receiver is 16 bytes above it.
In 64-bit LLInt, local zero is eight bytes below the frame pointer and `this` is
40 bytes above it. Ignition encodes the displacement in words. LLInt encodes
local `i` as `-1 - i` and uses a separate operand range for constants.
[Ignition frame constants](https://github.com/v8/v8/blob/main/src/execution/frame-constants.h)
· [Ignition register encoding](https://github.com/v8/v8/blob/main/src/interpreter/bytecode-register.h)
· [LLInt call frame](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/interpreter/CallFrame.h#L100-L172)
· [LLInt virtual register encoding](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/bytecode/VirtualRegister.h#L101-L119)

Cyallo uses an owned VM value stack. `FrameStack` stores frame headers and one
contiguous vector of optional values per running or parked task. Each header
has a slot range. The ranges partition the live vector in call order. Guest
calls reserve a tail range and fill parameters in place, including a closure's
leading captures or a method's receiver. Native recovery, module initializers,
and finalizers also initialize the active stack directly. Detached generators
own their slot buffers while suspended.

After verification assigns physical slots, `FrameLayout` prepares a
PC-indexed table for storage markers, copies and moves, scalar arithmetic and
conversions, branches, and returns. Those instructions index the active
frame's slot range with the prepared operand. Other instructions resolve
operands through `FrameLayout::slot`. Loans retain their logical frame and
register, so growing the vector can move its backing allocation. Recovery
and debugging use the same logical register IDs.
[Stack implementation](../src/bytecode/verified/run.rs)
· [Verified slot layout](../src/bytecode/verified/frame.rs)

Return truncates the live range after saving the result and its destination.
Truncation removes dead values from the active root scan and preserves capacity
for sibling calls. Existing bytecode drop operations control managed cleanup.
An instruction-budget pause or an input wait keeps the whole live range.
A yielding generator moves its frame values into detached storage and restores
them on resume. A parked task keeps its whole stack. The task's backing storage
is freed when the stack is dropped.

Start with a contiguous vector and measure execution after lowering as well as
whole-command latency. Consider segmentation if growth copies become material
or a native interface requires stable addresses. A segmented design should keep
each frame within one segment so the interpreter can resolve the segment once
per frame entry. Trim retained capacity at an idle boundary only if a
long-lived continuation exhibits sustained excess retention. Measure the cost
of the prepared operand table before extending its coverage. Measure generator
yield and resume before changing ownership of detached slots.

The first slice was measured on ARM64 macOS with warmed `hyperfine` runs of the
verified CLI. These wall times include process startup and bytecode lowering.

| Workload | Runs per binary | Per-frame buffers | Contiguous stack |
| --- | ---: | ---: | ---: |
| `fib(22)` | 90 | 23.9 ms | 22.6 ms |
| `fib(26)` | 40 | 124.7 ms | 116.3 ms |

The next slice was measured against the first slice in a separate session.
The measurements include both prepared slots and direct initialization for
the remaining active call paths.

| Workload | Runs per binary | First slice | Prepared slots and calls |
| --- | ---: | ---: | ---: |
| `fib(22)` | 90 | 22.6 ms | 20.4 ms |
| `fib(26)` | 40 | 121.4 ms | 103.6 ms |

The regression test grows the stack recursively while an exclusive loan
points into an ancestor frame. It collects on every allocation, resumes one
instruction at a time, and exercises both eager and deferred checking.
[Regression test](../tests/verified_borrowing.rs)

## 6. Verification gates for every slice
```bash
args=$(ls tests/verified_*.rs tests/vm_*.rs tests/cli.rs tests/native_emitters.rs tests/bytecode.rs | sed 's#tests/\(.*\)\.rs#--test \1#')
cargo test --release --offline --locked $(echo $args)
cargo clippy --release --offline --locked --lib
```
- No new failures; the three native test files must include recovery-at-every-budget runs.
- Run the same programs with `--gc-stress` once 4.4 lands.
- Each slice ends with a stage-doc paragraph and a commit whose trailer is
  `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`; stage only your own paths
  (`git commit -- <paths>`), because other sessions leave staged changes in the index.
- Record benchmark numbers (storage, startup, throughput) in the stage doc once the
  tiers cover the examples, before the legacy engine is deleted.

## 7. Review risks to check (from the review of `be6cc7f`, `08269f5`, `df22415`)

Committed HEAD passes the verified, vm, CLI, SDK-host and native suites in a clean export.
These points are not covered by a failing test; check them before building on this code.

1. **Native `Vec` is a hardcoded backend type.** `be6cc7f` gives the native tiers a dense
   vector layout and vector bridges. The project owner's direction is `Vec<T>` as ordinary
   std code over memory primitives (like Rust's `Vec::push`), not intrinsics or bytecode.
   Treat the native `Vec` as a stopgap: when `Buffer<T>` lands, the vector layout,
   bridges and loan relocation come out. Do not extend it (no `pop`, `VecDeque`, slices of
   vectors) in the meantime.
2. **Cost of the shared runtime.** At every bridge `08269f5` copies all initialized scalar
   homes into the interpreter's frame and back (`run_section`). It is correct but costs
   O(live scalars) per bridged instruction, so code that bridges often can be slower than
   the interpreter. Measure a loop with a bridged operation inside; if it loses, keep
   scalar state in the interpreter frame between consecutive bridges, or bridge in runs.
3. **Vector growth with live loans.** `relocate_vector_loans` rewrites spilled loan words
   that point into a buffer that grew. Add tests for: an element reborrow live across a
   push through the vector loan in an *inlined* Cranelift loop (not only a callee), several
   frames holding loans into the same buffer, and a loan that also lives in a Cranelift
   borrowed word. Anything holding an element address outside `Site::recover` is missed.
4. **Two driver modes.** Word-based plans (`WordActivation`) and shared-runtime plans
   (`SharedActivation`) have separate drivers, recovery paths and failure decoding, and a
   function picks its mode (`uses_shared_runtime`). Keep one conformance suite that runs
   every program in both modes, and decide whether the word mode stays once the shared
   runtime covers it, to avoid maintaining both.
5. **Uncommitted work in flight.** At the time of the review the working tree had about
   800 uncommitted lines in `src/bytecode/verified/run.rs`, `src/main.rs`, `src/vm.rs`,
   `src/backend/verified/{publish,run}.rs`, `src/bytecode/verified/frame.rs` and
   `src/report/model.rs`, and one `verified_composites` test failing only with them.
   Commit or discard that work before starting new slices on the same files.

Benchmark note (hyperfine, `examples/hot-loop.ya`, 100k-iteration scalar loop, 400 runs
each): `legacy_jit` 7.0 ms and 16.6 MiB, `ng_jit` 7.7 ms (+9.9%) and 14.0 MiB (-15.4%).
At this size wall time is dominated by startup (compile, lowering, tier publication), not
the loop, so the +0.7 ms is startup cost; use a longer loop (millions of iterations) and a
startup-only run to separate them before drawing conclusions.

## 8. Tier-up policy: data and proposal

Raw data: [native-tier-samples.md](native-tier-samples.md) (aarch64 macOS, release build,
collected by a sampling agent; all 24 native runs matched the interpreter).

Findings:
- Run cost per executed verified instruction: interpreter 7.9-9.8 ns; Winch 0.58-0.71 ns
  on scalar, float and array loops, 1.45 ns with calls, 2.0 ns on tuple/enum loops;
  Cranelift 0.22-0.23 ns scalar, 0.85 with calls, 1.36 on tuple/enum. The gap Winch minus
  Cranelift is 0.48 ns (scalar) to 0.65 ns (calls, enums).
- Cranelift `LowLatency` and `Optimized` run within 3% of each other on every program, and
  `Optimized` compiles 1.1-1.7x slower: promote to `LowLatency` only.
- Compile cost over plan sites `s` (instructions): Winch about 12 us + 0.37 us * s
  (linear: 1.7 ms at 4618 sites). Cranelift `LowLatency` is superlinear and fits
  `250 + 16 s + 0.005 s^2` microseconds within 5% (0.70 ms at 28 sites, 25 ms at 1162,
  178 ms at 4618).
- Cold start: the first emit in a process costs 0.5 ms (Winch) and 2.1 ms (Cranelift),
  against medians of 22 us and 0.70 ms: about 1.4 ms of one-off Cranelift setup.
- Startup of the examples is dominated by the frontend (`compile` about 3.1 ms); lowering
  35 us, preparing 4 us, Winch emit of the entry 11 us.
- Sampling is feasible: a paused `resume(100_000)` always reports function and instruction,
  and `status()` costs about 4 ns. Resume overhead is within noise (about 2 us).
- Threads: `PreparedFunction` and `UnlinkedImage` are `Send`; `CodeStore` is not (its
  `EntryPin` holds an `Rc`). A worker can therefore emit, and the main thread publishes.

Implemented in `src/backend/verified/tiering.rs` (`Tiered`, `TierPolicy`), used by
`ng_jit` through `vm::start_jit`; `CYALLO_TIERING=off` disables it and
`CYALLO_TIER_REQUEST_PERCENT=n` sets how early the compile is requested. Tests:
`tests/verified_tiering.rs`. Measured with `hyperfine -N` (no `env` wrapper: it adds
about 3 ms to every run) on `examples/osr-loop.ya` (27M instructions) and
`examples/hot-loop.ya`:

| | osr-loop | hot-loop |
| --- | --- | --- |
| `ng_jit`, tier-up off | 25.1 ms | 7.6 ms |
| request at 100% of the threshold | 15.0 ms | 8.0 ms |
| request at 50% | 14.4 ms | 7.6 ms |
| request at 25% (default) | 14.0 ms | 7.7 ms |
| request at 10% | 13.5 ms | 7.7 ms |
| `legacy_jit` | 11.4 ms | 6.8 ms |

The compile runs on a worker thread, so requesting early costs CPU on another core, not
latency, and the worker is not joined at exit. The default is 25% of the threshold; 10%
is a little faster but compiles almost any function that runs for 0.2 ms, which would
waste CPU on a busy or single-core machine. The remaining gap to `legacy_jit` is the
Winch time before the swap, the Cranelift cold start (1.4 ms) and +3 MiB for the worker.
Policy as built (the threshold is when the compile is worth it; with `request_percent`
the request goes out earlier):
1. Run everything in Winch first (cheap, one-to-one with the bytecode).
2. Slice the run at 100k instructions. At each pause attribute the slice to the function
   at the reported position (`executed[f] += slice`). No counters in generated code.
3. Promote function `f` with `s` plan sites when its executed instructions reach
   `N(s) = 500_000 + 32_000 * s + 10 * s^2`, which is the point where the saving of
   0.5 ns per instruction repays the estimated Cranelift compile cost
   (`1.4M` at 28 sites, `11M` at 298, `51M` at 1162). Use the executed count so far as the
   estimate of what remains. Add 1.4 ms to the cost for the first Cranelift emit of the
   process, or warm Cranelift on the worker once any function reaches half of its threshold.
4. Emit `LowLatency` on a worker thread from a clone of the `PreparedFunction`; publish the
   image on the main thread at the next pause. Both tiers share the plan, frame layout and
   recovery maps, so the swap is re-entering the new image at the paused instruction
   (this is also the loop OSR for a single long invocation).
5. Programs shorter than about 10 ms never reach a threshold and pay nothing.

Open points: calibrate the constants on a second host (x86_64) and on call-heavy and
enum-heavy code; decide whether large functions (`s` over about 1000) should be split or
skipped because their threshold exceeds typical run lengths; revisit after fuel batching
and lowering cleanups change the per-instruction costs.
