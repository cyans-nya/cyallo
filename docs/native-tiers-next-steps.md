# Finishing the native tiers: instructions for the next sessions

State at commit `bd149a6` (native slices 1, 2a, 2b, 3a, 3b, 3c done). Goal from the
project owner: **any code must be able to run in the native tiers**; the interpreter
stays the reference and the fallback. Native work does not block the legacy removal
([legacy-removal-plan.md](legacy-removal-plan.md)). Read
[compiler-and-runtime-rules.md](compiler-and-runtime-rules.md) first: typed errors,
declaration-based primitives, instance-owned runtime state.

## 1. How the native tier works today

Code map (all under `src/backend/verified/` unless noted):

| File | Role |
| --- | --- |
| `prepare.rs` | Legalizes verified bytecode into a `PreparedFunction`: `Operation`s per instruction (`Site`), the frame map (`FrameMap`), layout table (`Layout`), word table (`WordOp`), bridge table (`BridgeCall`), array accesses (`ArrayAccess`), call table. **Anything not legalized returns `UnsupportedOperation`/`NonScalar` and the routine stays in the interpreter.** |
| `cranelift.rs`, `winch.rs` | Two adapters that emit machine code from the same plan. Every `Operation` needs both. Cranelift keeps frame words in SSA variables except *borrowed* words (see 3); Winch keeps every word in frame memory. |
| `run.rs` | The driver: runs one native function per invocation, handles calls (copying words between frames), returns, failures, bridges, and recovery into the interpreter (`NativeActivation::recover`). Owns the `Heap` (strings, array buffers) and the output sink. |
| `abi.rs` | `ScalarOutcome` (Returned, Paused, Failed, Call, Bridge), `Failure` codes. |
| `publish.rs`, `validate.rs`, `image.rs` | Code store, linking, image checks. |
| `src/bytecode/verified/run.rs` | The interpreter (reference). Native recovery enters through `restore_scalars`, `RecoveredValue`, `RecoveredStep`. |

Execution model: a frame is an array of 64-bit words. Scalars take one word; tuples/
enums of scalars take consecutive words (`Layout`); a string is an index into the
activation's string table; an array is the address of a buffer the activation owns; a
loan is an address (element loans have the low bit set). A native function exits with
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
3. **Legalize** in `legalize()` (`prepare.rs`): produce an existing `Operation`, a
   word-move (`Operation::Words`), or a new `Operation`/`BridgeCall`. Remove the
   instruction from the rejection list at the bottom. Reject (`return Err(unsupported)`)
   every shape you do not handle, e.g. a destination that overlaps a source.
4. **Failures.** New failure kinds need a `Failure` code (`abi.rs`), the `failures()` table
   in `prepare.rs`, and the mapping to the interpreter's `RunError` in `run.rs`.
5. **Both adapters.** Cranelift (`cranelift.rs::operation`) and Winch
   (`winch.rs::operation`). Winch has few scratch registers (`LEFT RIGHT VALUE TEMP
   ADDRESS SAVED_DIVIDEND`); `address()`/`through()` use `ADDRESS` for large offsets, so
   do not hold values there across `load_home`/`store_home`.
6. **Recovery.** If the instruction produces a new kind of register value, extend
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

### 4.1 Vectors and ranges (next)
Two collections, two representations (project owner's decision):

- **Array (like a JS array, may have holes).** Fixed size, per-element *written* flag,
  as implemented in slice 3c: two words per element, `UninitializedElement` on reading a
  hole. Element loans are tagged addresses so a write through one fills the hole.
- **Vec (like a Rust `Vec`).** Dense and compact: no holes, no flags, elements stored
  back to back exactly like Rust. A native Vec register is one word, the address of a
  heap block `[length, capacity, element 0, element 1, ...]` owned by the activation.
  Elements take **one word each** at first; once the layouts exist, pack by element type
  (`bool` as one byte, `i32` as four, composites of scalars inline at their layout size)
  so that a Vec of plain values is as compact as in Rust. Element loans are plain
  addresses (tag bit clear), since an element can never be a hole. Growth reallocates
  (`VecPush` is a bridge or an inline fast path when `length < capacity`); the borrow
  checker already forbids a live element loan across a push, so no loan fix-up is needed.

Instructions: `VecNew`, `VecFrom` (range to vector), `VecPush`, `VecLength`, and
`ArrayLoad`/`ArrayStore`/`LoanElement` on vectors (`body.is_vec`/`projects_from_vec`
select those paths; the bound is the length word, not a static range). `Clone` of a vector
is a bridge deep copy. Ranges of integers are two words, so make them an inline
`Layout::Range`: `RangeNew`, `RangeInclusive` (copy the interpreter's overflow check on
the closed end).

Ownership as for arrays: copy is a bridge deep copy, by-value call arguments are
deep-copied in the driver, `Move` transfers the address, `StorageDead` of an initialized
Vec releases the block. Recovery: `RecoveredValue::Vec(Vec<Scalar>)` (the interpreter's
`Composite::Vec` holds initialized elements only). Do `for x in range` loops first: they
dominate `.ya` programs. Vec elements that need drop (text, `rc`, `gc`) wait for 4.4.

### 4.2 Cells, slices, sums, extrema, remaining text
- `CellNew/Read/Write/Replace` of scalar contents: a cell is its content inline
  (`Layout::Cell` = element layout); reads and writes are word moves; shared-loan writes
  work because loans are addresses.
- `SliceOf`, `SliceLength`, `Sum`, `Extremum`: a slice is a window (address, length,
  start) over an array or vector; keep it as a loan-like pair of words and decide how a
  `Slice` loan recovers (`Window` in the interpreter place).
- `TextTest`/`TextMap` already bridged; add the not-yet-covered text instructions and
  arrays of text only after 4.4 (they need element drop semantics).

### 4.3 Closures, indirect calls, bound calls (host functions), method calls
A closure value is a pair `(function id, captures)`. Captures are words when scalar;
store the capture words in an activation heap block and let the closure register be
that block's address. `Closure` builds it (bridge). `CallTarget::Closure` needs the
**callee's native entry from the code store**: give `NativeActivation` a reference to the
store (or a resolver) so the driver can pick the entry (or fall back to interpreting that
frame, see 4.6). Captures are the callee's leading parameters. `CallTarget::Bound` (host
bindings) and `CallTarget::Method` (trait objects, service views) go through the existing
interpreter implementations via the bridge mechanism of 4.6.

### 4.4 Counted owners and GC handles (root maps)
`Rc`, `Weak`, `Gc` need the collector to see native frames.
- Represent `Gc` as the interpreter's heap index (a word) and keep the interpreter's `Heap`
  inside `NativeActivation` (move `Memory` ownership into the native activation;
  `ActivationMemory`/`into_memory` already show the pattern).
- Add `Site::roots` (per instruction: which homes hold handles) and make allocation
  bridges (`GcNew`, `RcNew`, `TraitBox`, `Allocate`) the only points that collect;
  they run in the driver where all native frames are known, so the root set is the union
  of `recover` homes holding handles across the whole stack plus the heap.
- `--gc-stress` (collect before every allocation) must pass for every native test; add a
  `gc_stress` flag to the test helpers.
- `Drop`/`Clone`/`RcClone`/`RcDowngrade`/`RcUpgrade`/`RcStrongCount` become bridges that
  call the interpreter's `memory.drop_value`/`clone_value` on the handle.
- Finalizers (allocator-backed blocks) push frames in the interpreter (`memory.finalizers`);
  a bridge that frees such a block must hand control to the interpreter (recover) or run
  the finalizer natively; start with recover.

### 4.5 Input, module variables, random, clocks
- `Input`/`InputLoan`: give `NativeActivation` an `InputSource`. When no line is waiting the
  interpreter reports `AwaitInput`; native must exit with a paused status the same way
  (the CLI/`vm::Session` already handle `ExecutionStatus` for the interpreter; mirror it).
- Module variables (`BorrowGlobal`, loads/stores of globals): a per-activation words block
  addressed through the context; initialization state per variable (see
  `UninitializedGlobal`).
- `Random`: seeded LCG state lives in the activation; bridge it.

### 4.6 The general fallback: execute one interpreter instruction from native code
For everything else (objects, erased values, services, module loads, generators, tasks,
channels) do **not** reimplement semantics. Add a bridge that recovers the current stack
to an interpreter `Activation`, runs exactly one instruction there, and re-imports the
frames. This needs: (a) a way to step the interpreter one instruction with `fuel = 1`,
(b) a `NativeActivation::import(activation)` that rebuilds native frames from interpreter
frames for the instruction's kinds, (c) keeping non-word registers in a side table with
words as indices. Do this only after 4.1-4.5 so the common kinds do not pay for it.
Native park/resume for generators, tasks and channels (stage 8 in `overhaul-stages.md`)
builds on this: a parked generator is a suspended frame stack, which the interpreter
already represents.

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
`u64` counts (`ScalarType::U64`) are rejected in `FrameMap::home`; add them (one word,
unsigned compares). Chars and 32-bit numbers need verified kinds first (legacy-removal
decisions 1-2). The simplified dialect needs sparse arrays in the verified engine first.

## 5. Interpreter-side prerequisites to remember
- `Activation::restore_scalars` is the only way native frames re-enter the interpreter;
  it validates storage state and shapes. Each new native value kind needs a case there.
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
