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
