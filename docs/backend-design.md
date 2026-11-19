# Verified native backend design

The replacement runs one verified bytecode format through a resumable interpreter
and two native adapters. Winch emits the initial native tier. Cranelift emits
the low-latency and optimized native tiers. Compile and test the replacement in
slices. Switch shipping entry points together once the new pipeline meets the
language, runtime, and SDK contracts. [Execution cutover](compiler-runtime-overhaul-plan.md#direct-execution-cutover)

The [Winch and Cranelift implementation plan](jit-implementation-plan.md)
defines stage owners, prerequisites, safe parallel work and acceptance tests.

The current `Program` packages legacy bytecode. The opt-in `VerifiedModule`
lowers a selected checked routine and its transitive direct callees. Its
scalar interpreter uses verified reusable frame slots and returns a fuel error
when it exhausts its total fuel. Its owned activation accepts a separate slice
budget and preserves verified frames and the next instruction between slices.
It executes selected scalar routines through the verified interpreter
interface. The runtime entry points still need the complete verified
interpreter before cutover. [Program packaging](../src/compiler.rs) · [Verified lowering](../src/bytecode/verified/lower.rs) · [Scalar runner](../src/bytecode/verified/run.rs)

The opt-in scalar code store prepares functions from that verified module,
validates relocations and metadata, and publishes native entry pins. Native
call handoffs consume the resolved address from the linkage table. The
dispatcher uses the retained callee pin and records the caller continuation.
A paused mixed-tier activation can transfer both frames to the verified
interpreter with its original fuel count. [Code store](../src/backend/verified/publish.rs) · [Native activation](../src/backend/verified/run.rs)

## Modules and dependencies

```text
checked IR → verified bytecode → resumable interpreter
                     │
                     └→ shared native preparation → Winch or Cranelift
                                                          │
                                             unlinked code and metadata
                                                          │
                                      validate → publish → pinned entry
```

| Module | Owns | Interface |
| --- | --- | --- |
| Verified bytecode | Logical registers, storage, ownership, operations, failure and continuation identities, instruction origins | Immutable verified functions and typed descriptors shared by the interpreter and preparation |
| Native preparation | Logical optimization, native admission, target legalization, frame requirements, ABI classification, recovery obligations | A verified function plus immutable dependency and feedback snapshots produces a verified native plan or a typed admission result |
| Winch and Cranelift adapters | Instruction selection, register allocation where applicable, frame placement, relocations, machine code offsets, actual value locations | Each accepts a whole native function plan and produces an owned unlinked code image |
| Code publisher | Image validation, typed symbol resolution, executable memory, instruction-cache synchronization, atomic installation and retirement | A candidate image and its original obligations produce a pinned executable entry or a typed publication error |
| Runtime execution | Interpreter frames, native activation pins, scheduling, installed SDK bindings, collection, RC, and cleanup | A runtime-owned dispatcher invokes the prepared entry or verified interpreter and applies the resulting continuation |

Terms follow [SDK registration](sdk-registration.md#terminology) and the
[component worlds](component-worlds/03-worlds-and-sdk.md): an **SDK** declares
bodyless items, a **provider** is the runtime or embedder that implements them,
a **binding** is the contract for one SDK item, and an **installed table** is the
immutable result of installing a provider's bindings against an SDK. A **bound
call** targets an SDK item through its installed binding. This document does not
use "provider call" or "installed provider" for those.

**Tiers.** Winch serves the low-latency baseline tier. Cranelift has a
low-latency profile and an optimizing profile for hot routines. Both consume
the same prepared plan. After independent conformance, the verified
interpreter is the executable reference and runs routines ineligible for
native compilation.

Preparation and image validation are in-process modules with independently
testable inputs. Winch and Cranelift are two adapters at the emission seam.
Executable mappings and the effects of bound SDK items enter through internal runtime
interfaces. Integration tests use the production publisher and a controlled
provider. Fault tests can substitute a mapping failure or a deterministic
provider at the same internal seam. Compile workers borrow only immutable
inputs. The existing process-wide compile worker pool supplies capacity under the rules in
[Compile worker capacity](#compile-worker-capacity).
Dependency snapshots, feedback, installed bindings and published code belong to
their compiler or runtime owner. [Compiler and runtime rules](compiler-and-runtime-rules.md)

## Compiler and runtime interfaces

The following Rust sketches describe the interfaces. Field names and module
placement are proposed.

```rust
struct PrepareRequest<'a> {
    function: VerifiedFunctionRef<'a>,
    dependencies: &'a CompileDependencies,
    feedback: Option<&'a FeedbackSnapshot>,
    target: &'a NativeTarget,
    policy: &'a CompilePolicy,
}

enum NativeAdmission {
    Prepared(PreparedFunction),
    InterpreterOnly(NativeIneligibility),
}

fn prepare(request: PrepareRequest<'_>) -> Result<NativeAdmission, PrepareError>;

fn admit<'a>(
    function: &'a PreparedFunction,
    backend: &'a BackendDescriptor,
) -> Result<AdmittedFunction<'a>, NativeIneligibility>;

trait NativeEmitter {
    fn descriptor(&self) -> BackendDescriptor;
    fn emit(&mut self, function: AdmittedFunction<'_>)
        -> Result<UnlinkedCodeImage, EmitError>;
}

fn validate_image(
    prepared: &PreparedFunction,
    image: UnlinkedCodeImage,
) -> Result<ValidatedCodeImage, ImageError>;

impl CodeStore {
    fn publish(&mut self, image: ValidatedCodeImage)
        -> Result<Arc<PublishedCode>, PublishError>;
}
```

`VerifiedFunctionRef` pairs a function ID with its owning immutable verified
module. `PreparedFunction` owns its tables and identity, and has a private
constructor. It carries the target and ABI revisions used to build it.
`BackendDescriptor` declares the target, available machine features, and
supported plans, including entry kinds and map kinds. Preparation derives
the required features independently of a particular adapter. Admission
compares those requirements with the descriptor and issues a scoped token
for the chosen adapter. One prepared function can therefore be emitted by
both qualifying adapters on the same target. Worker-local scratch stays in
the adapter. The descriptor is an owned handle to immutable target facts.
`UnlinkedCodeImage` owns bytes, sections, relocations, entries and metadata.
`ValidatedCodeImage` has a private constructor and retains the checked
dependency and ABI identities. Publication revalidates them against the live
runtime immediately before installation.

The caller that owns a `Session` goes through an execution dispatcher. Its
normal operation takes a verified function, an owned or borrowed argument
bundle, a processor lease and fuel. It resolves a published entry using the
instance identity and execution policy, acquires the code pin, and enters the
activation. Missing, stale or declined native code selects interpretation
before native entry. The normal interpreter can request compilation for a
later invocation. Compiler workers return unlinked results across a runtime
mailbox. The dispatcher receives publication state through its runtime owner.

A development fixture and a compile worker use the same sequence.

```rust
match prepare(request)? {
    NativeAdmission::Prepared(plan) => {
        let descriptor = emitter.descriptor();
        match admit(&plan, &descriptor) {
            Ok(admitted) => {
                let candidate = emitter.emit(admitted)?;
                let checked = validate_image(&plan, candidate)?;
                deliver_to_runtime(checked);
            }
            Err(reason) => record_admission(reason),
        }
    }
    NativeAdmission::InterpreterOnly(reason) => record_admission(reason),
}
```

| Outcome | Owner and treatment |
| --- | --- |
| Bytecode verification failure | Compiler error for compiler-generated bytecode. The compiler preserves the failing function and origin. |
| Native ineligibility | A typed admission reason for a verified routine such as a target limitation, policy decision or unsupported statepoint. Interpreter execution uses the same verified function. |
| Emission or image validation failure | A typed backend defect or resource failure at the failing step. Preserve its cause and surface it through compile reporting. |
| Stale publication candidate | The owning runtime rejects installation. A new request takes a fresh immutable dependency snapshot. |
| Guest return, trap, budget handoff, pending operation or cancellation | A typed execution outcome with the owning activation and required continuation. A returned language `Result::Err` remains a guest value. |

## Compile worker capacity

Decision: the pool stays process-wide, as recorded, but it is a capacity
provider only and owns no instance state.

1. A job carries the instance identity, the immutable inputs and a cancellation
   token. The pool and its worker-local scratch retain nothing from a job once it
   returns.
2. Results return only through the owning runtime's mailbox. The runtime decides
   publication. The pool never installs code, resolves bindings or reads feedback.
3. Each instance has a named cap on queued and in-flight jobs. Runtimes that
   share an explicit compile admission capability take turns round-robin
   within that capability. It owns the ready queue and cursor. Independent
   capabilities share worker capacity and keep their own admission state.
4. Dropping or resetting an instance cancels its queued jobs and discards its
   late results on delivery, using the instance identity.
5. Pool size, the per-instance caps and the queue order are named fields of the
   compile policy, with the owner and reason stated.
6. Two instances in one process must produce identical code for identical
   inputs. A test compiles the same function from two instances concurrently and
   compares the images and the published identities.

## Native preparation and emitter contract

`PreparedFunction` contains typed blocks and operands, logical instruction
origins, frame objects, prepared calls and the obligations for each exit and
statepoint. Operand and table IDs are scoped to the plan. Each stage checks
identity before using offsets or resolving symbols. The verifier checks block
arguments and branches, widths, projections, initialization, loan and cleanup
facts, portable fuel costs, and complete continuation edges as features enter
the supported subset.

| Prepared fact | How both adapters use it |
| --- | --- |
| Type, layout and projection | Use the target layout service for size and alignment. Resolve projections to typed displacement, access width and alignment. Carry the required alias and barrier facts with each access. Enum layouts derive from instantiated shape and target constraints. |
| Stack object | Place a typed object with size, alignment, live and initialized regions, cleanup, address exposure, and relocation class. Choose a machine register or stack slot where the contract permits it. |
| Checked operation | Emit the selected scalar or memory operation with explicit success and failure successors. Preserve the failure kind and bytecode origin. |
| Call | Use a prepared `FnAbi`, argument/result locations, passing modes, and the outcome edges. A closure call carries the actual environment as a hidden operand. A bound call to an SDK item uses the contract ID of its installed binding and a typed runtime entry. |
| Statepoint | Bind each required symbolic value and frame object to an actual register, stack location or approved reconstruction recipe at the final machine PC. |
| Fuel and origin | Charge canonical bytecode work and preserve logical and inlined origins. Reconstruct the exact next instruction on a budget handoff. A grouped check needs a proof that it preserves the defined stopping points. |

The first ABI covers scalar guest calls and their success and failure outcomes.
Use one target-specific guest convention across both adapters. Classify
arguments using the target ABI, carry an opaque invocation context and an
environment operand, and use an uninitialized result place plus a validated
outcome discriminant. The capture-free case has a named empty environment
representation. Success initializes the result place. A failure carries its
typed reason and origin through the invocation context. The ABI schema owns
record sizes, field offsets, strides, alignments and discriminants. Check its
host-facing declarations with `repr(C)`, `offset_of!`, `size_of` and
`align_of`. Keep the result and activation representations shared by both
adapters.

The runtime call table uses typed operation IDs and versioned signatures. A
collecting, parking or reentrant call transitions through a checked bridge.
Preparation supplies the operand and result layout, ownership commit points,
statepoint, and success, failure and pending continuations. Host capabilities
are resolved from the instance's installed SDK bindings. A compiler snapshot carries
binding identities and versions for validation.

The frame planner may reuse storage once its old value is dead, its loans are
ended, and its cleanup is complete. Trivial non-addressable values can die at
last use. An address-exposed place needs a stable home for the life of its
loans, including across supported handoffs. Interpreter frame slots and native
stack objects use the same logical lifetime facts with their respective
placement rules. General payload-loan proofs and the legacy cell release
mechanism remain open. [Layout decision](pending-decisions/03-layout-optimizer.md) · [Scope reuse decision](pending-decisions/06-memory-lifetimes.md)

One shared optimizer selects calls and primitive operations using resolved
identities. A resolved operator trait call is eligible for eager inlining
under the same effect, ownership and budget rules as an ordinary call. The
inliner remaps locals, storage and cleanup scopes, source origins,
statepoints, effects and fuel when inlining. Observed runtime targets inform
guards. A guard uses the evaluated callable, receiver and arguments. The
fallback is the generic checked operation in the verified pipeline. A later
miss reconstructs the recorded continuation after any effects that committed.
Closure feedback can specialize a code target while the environment remains a
runtime operand.

Production calls validate the callee's instantiated signature and contract
at caller verification. Preserve deferred-body admission at the language's
specified first use. A compile request names the admitted function instance.
The scalar prototype's transitive direct-call lowering is useful for closed
fixtures. Cross-adapter linkage uses the same ABI and keeps logical identity
separate from each artifact's local IDs. [Current scalar lowering](../src/bytecode/verified/lower.rs)

## Images, publication and recovery

A code identity includes the runtime or compiler owner, program snapshot,
function instance, tier, target and ABI versions, and dependency versions.
The request also records the facts used for specialization so they can be
checked at publication and where required at entry. Publish a complete image
transactionally. The validator checks section and entry ranges, alignment,
relocation kinds and targets, frame layout, machine locations, and complete
metadata for every required statepoint and exit. The publisher resolves typed
symbols, changes executable permissions and synchronizes the instruction
cache before exposing the entry. The code store pins images for active and
suspended invocations and reclaims retired code after the last pin is gone.
Installation and entry acquisition synchronize with dependency updates.
Acquiring the pin and selecting the generation are one runtime operation.
For deferred linking, retain a typed entry indirection until a callee has
validated code. A closed scalar fixture can link direct calls between
co-pinned images. Resolve SDK binding dependencies against the owning runtime so
equal scoped numeric IDs from different runtimes cannot be interchanged.

Finalize an image after register allocation and branch fixups. For each
required statepoint and exit, tie the final PC and its locations to the
obligation issued by preparation. Validate that locations are representable,
in range, and writable when they need repair, with the right ABI width and
ownership. An adapter can materialize a value in a known spill slot if it
cannot report its live machine location. Enable an adapter feature once it
can produce the required maps. The publisher checks declared structures and
obligations. Adapter conformance tests check that the emitted instructions
actually implement them.

| Statepoint metadata | Required relationship to the plan |
| --- | --- |
| GC roots | Every live traced value has a rooted location or checked reconstruction recipe. Derived references record how to reload after relocation. |
| Stack relocation | Live references into the stack have exact, writable repair locations. Saved stack pointers, frame links, and live register references are included. |
| Ownership and cleanup | Record initialized and moved paths, active variant and ownership state, outstanding cleanup and its phase. |
| Deoptimization and OSR | Preserve logical and inlined frames, their versions, value recipes, next instruction, and effect commit phase. |

Treat a statepoint as a handoff of one exclusive activation. Before an SDK
operation can park, register its identity, result destination and successor.
The runtime owns completion and cancellation arbitration. Retain owned payloads
or rooted handles across a pending operation. The resumed activation consumes
the outcome through the recorded successor. Ownership transfer updates
initialization and cleanup together. A park after transfer resumes with the
updated ownership phase. [Runtime concurrency contract](concurrency.md) · [Invocation root scopes](adr/0005-resumable-root-scopes-and-gc-capabilities.md)

The initial interpreter separates a resumable scheduler budget from a
terminal execution limit. A budget handoff returns the same activation and
the next logical instruction. Debit fuel before starting the corresponding
logical operation, including storage markers and calls. If native code cannot
complete the next verified emission region with the available budget, hand
off at that region's entry. When deoptimizing after a committed call, transfer
its returned value or pending operation ID and resume at its successor. An
OSR import prepares the destination and acquires its code pin before moving
ownership out of the source activation.

Go-style copying stacks are a later extension. Park or stop the coroutine
exclusively at a statepoint, prepare the new stack, repair every mapped
reference, and publish the new saved context together with the new stack. Host
and Rust calls run on the system stack. Host pending state retains owned
buffers or rooted values. Internal references that must survive outside the
stack use an activation, slot and generation with a verified projection.
The runtime validates the reference when resolving it. A parked coroutine
releases its processor lease and reacquires one before executing. Generator
yield preserves its separate resumer contract. Enable stack copying only
after relocation, root scanning
and cleanup tests exercise each supported statepoint. Enable OSR only after
checked frame import/export and exact effect recovery are available.

The invocation root scope survives budget exits, tier changes and parking.
Reload traced and derived references after collection from the recorded
roots. Trace initialized fields of RC payloads, aggregates and closure
environments. Destroy only the active initialized variant payload. On last RC
release, commit strong ownership loss before scheduling destruction so weak
upgrade observes it. Retain the control block, allocator state and remaining
cleanup phase if destruction parks. Stack copying and frame import transfer
existing ownership and cleanup obligations.

## Interface alternatives reviewed

| Interface | Leverage and trade-off |
| --- | --- |
| Whole-function preparation and emission | The same plan goes to Winch and Cranelift. A compact request and owned image keep target-specific representations private. The extra common representation earns its cost by centralizing ABI, layout, fuel and recovery obligations. |
| Staged emission session | A scoped draft can enforce valid operand IDs and capture statepoint bindings as the adapter emits. Exposing every phase to the worker would make phase ordering and adapter internals part of its interface. Keep draft creation and late finalization inside the backend module. |
| Runtime executable facade | CLI, Session, workers and embedders get a common entry and publication contract. Its depth comes from coordinating verified preparation, admission and code lifetimes. Keep emission, registry policy and scheduling in their owning modules so the facade only orchestrates the flow. |

Use the whole-function interface for adapters, shared internal finalization
for statepoint and frame metadata, and the runtime execution dispatcher for
ordinary callers. This gives compilation callers one immutable request and
runtime callers one entry that owns version selection and pinning.

## Structure and boundaries

The previous bytecode and backend grew into one tangled layer where the
format, the interpreter, optimization and native emission all knew each other's
internals. This design keeps them apart, and a change that crosses a boundary is
a design change, not an edit.

```text
cyallo-bytecode    format, codec, verifier, disassembler        (depends on nothing below)
cyallo-interp      resumable interpreter                        (bytecode)
cyallo-native-prep preparation, ABI, layout, obligations        (bytecode)
cyallo-emit-winch  Winch adapter                                (native-prep)
cyallo-emit-clif   Cranelift adapter                            (native-prep)
cyallo-codestore   image validation, publication, pins          (native-prep)
runtime            dispatcher, scheduling, bindings, GC         (all of the above)
```

Dependencies point one way, down the list. Crate boundaries enforce this, so a
module cannot reach upward by accident. Crate names are proposed.

1. **One definition per operation.** Each bytecode operation is declared once in
   a single table that generates the codec, the disassembler and the verifier's
   shape checks. The interpreter, preparation and the adapters each implement
   against that table. Nothing re-derives an operand layout or a cost.
2. **One format, no parallel paths.** The verified format replaces the legacy
   one at the cutover. Do not keep a second format, a translation layer between
   formats or a compatibility mode in the new crates.
3. **A stage reads only its input type.** The interpreter and adapters take
   verified or prepared values, never source, AST or runtime state. Each stage
   has tests that construct its input directly, with no earlier stage running.
4. **The runtime owns policy, not mechanism.** Scheduling, version selection and
   pinning live in the runtime. Emission, layout and validation live in their
   crates, and the runtime reaches them only through the interfaces above.
5. **Adapters stay thin.** Anything both adapters need, such as layout, ABI
   classification, fuel costs and recovery obligations, lives in native
   preparation. An adapter holds only instruction selection and placement. Any
   code copied between the two adapters moves to preparation.
6. **Each crate states its contract.** It has a short README naming what it
   owns, what it takes and returns, its invariants and what it must not depend
   on. A change that alters the contract updates the README in the same commit.
7. **Files follow concepts.** A file holds one concept and its tests sit next to
   it. When a file mixes two stages or two concepts, split it before adding more.

## Implementation rules

1. Every ABI displacement, field width, frame offset, tag, register convention
   and alignment comes from a named target or layout definition. Keep literal
   instruction encodings in the assembler or codec. Treat source immediates
   as typed operands. Give tuning constants names and a policy owner.
2. Use typed operation, layout, call, statepoint, continuation, relocation and
   error identities throughout. Resolve source names and SDK declarations and
   bindings before emission. A new semantic operation needs a checked IR/bytecode
   contract and a conformance case.
3. Put source semantics, ABI preparation, layout and recovery obligations in
   shared modules. Put Winch and Cranelift instruction selection and placement
   in their adapters. Runtime access goes through auditable typed bridges.
4. Confine executable memory, entry invocation and ABI pointer operations to
   focused platform and runtime modules. Each unsafe operation documents its
   required facts and where they were checked. SDK emission uses checked
   operands and typed operations through the safe emitter.
5. Verify unlinked images before publication and validate live dependencies at
   installation. Treat structural validation as evidence about structure. Test
   native instruction semantics independently with explicit fixture
   expectations, including failures and effects.
6. Make every stage inspectable. Given a routine and code version, a developer
   can dump bytecode, the prepared plan, ABI fields, admission reason, and
   native locations and statepoints. Use the same IDs in each dump so a
   source instruction or missed obligation can be traced through emission.

## Code quality rules

Readability, auditability and correctness come before speed. Measurements go in
[benchmarks](benchmarks.md) with the environment recorded. They inform decisions
and do not gate merges. Complexity added for speed needs a measured case written
next to the code. These rules apply to all code this design adds, and a reviewer
checks each one. Slice 1 adds a mechanical check for the first three where a tool
can enforce them.

1. **No magic numbers.** Outside tests, every numeric literal other than `0` and
   `1` used as a count or an identity is a named constant or an enum
   discriminant. It has a doc comment naming its source: a target manual, the ABI
   schema or a named policy. Sizes, offsets and strides come from layout
   definitions and `offset_of!`/`size_of`. Tests may state expected values
   literally, because the literal is the assertion.
2. **No magic strings or tags.** Kinds, statuses, reasons and symbol classes are
   enums. Never classify by formatted text. See the
   [compiler and runtime rules](compiler-and-runtime-rules.md).
3. **No lossy conversions.** Width and offset conversions use checked
   `TryFrom` with a typed error. Do not use `as` for narrowing or sign changes.
   A wrapping or saturating operation is a named choice with a reason.
4. **Unsafe is confined.** It lives only in the focused platform and runtime
   modules. Each block has a `SAFETY:` comment naming the facts relied on and
   where they were checked. Build with `unsafe_op_in_unsafe_fn` denied and
   Clippy's undocumented-unsafe lint enabled.
5. **No hidden panics.** Compiler and runtime paths return typed errors. An
   unreachable state uses a typed internal-error variant or `unreachable!` with a
   comment naming the verifier fact that excludes it. No `unwrap` or `expect`
   outside tests, and no unchecked indexing driven by input.
6. **Private constructors carry invariants.** A type whose validity matters, such
   as `PreparedFunction` or `ValidatedCodeImage`, documents its invariants and can
   only be built by the stage that establishes them.
7. **One responsibility per unit.** A function does one thing, and a long one is
   split by named steps, not by comments. Names state the domain meaning. Public
   items document their contract, failure cases and ownership.
8. **Determinism.** No behavior depends on hash iteration order, addresses,
   thread timing or the clock. Use ordered maps where order is observable. Tests
   do not sleep or race.
9. **No dead or speculative code.** No commented-out code, unused feature flags
   or parameters kept for a later slice. A `TODO` links to an entry in
   [pending decisions](pending-decisions/README.md).
10. **Every obligation has a negative test.** For each check a validator makes,
    a test supplies the malformed input and asserts the exact typed error.
    A skipped or ignored test names the pending decision that blocks it.
11. **Lints are clean.** New modules build with `clippy -D warnings`. Existing
    warnings are not widened. Formatting follows the repository's decision in
    [performance and checks](pending-decisions/08-performance-and-checks.md).

## Remove executable no-ops before interpreter conformance

The verified v3 dump for `cyallo bytecode examples/functions.psc --verified 1`
contained six `nop` instructions. PCs `0000`, `0011`, and `0016` marked source
statements. PCs `0001`, `0017`, and `0020` stand in for address calculations
eliminated when `n` was placed in a frame register. Both arrive as metadata or
empty work in the checked IR. Version 4 lowers these through the following
stages. [Frame-local placement](../crates/cyallo-ir/src/analysis.rs#L506) · [Verified lowering](../src/bytecode/verified/lower.rs)

1. Form typed blocks from checked IR targets and terminators. Record source
   statement boundaries as debug metadata and omit eliminated address
   calculations. Preserve each removed target's successor by forwarding its
   block label. Prune blocks unreachable from entry. Report a typed lowering
   error if a reachable path falls through the end of the function.
2. Compute liveness on that graph. Insert storage transitions and edge
   cleanup while targets still refer to block labels. Keep cleanup on the
   branch that requires it, including loop backedges.
3. Assign executable PCs after insertion. Resolve branches, instruction
   source spans and debugger stops together. Anchor a statement to the
   verified location where the debugger should stop before its work begins.
   Preserve source order and control-flow provenance when several statement
   boundaries share a location. Debug metadata consumes no fuel.
4. Remove `Nop` from the verified instruction schema and increment the
   format version. Executable storage transitions continue to consume fuel.

The regression tests check zero `nop` instructions, factorial results `1`
and `120` for inputs `0` and `5`, and resumption with one-instruction slices.
They exercise forwarded labels, backedges, and statement markers visible at
an activation pause. The debugger can query those markers without
advancing the activation. Integration with the shipping debugger follows the
verified execution cutover. [Scalar integration tests](../tests/verified_bytecode.rs) · [PC layout](../src/bytecode/verified/layout.rs)

## First implementation slice and cutover tests

1. Exercise the resumable scalar activation with bounded fuel, direct calls
   and failure spans using a checked source program. Remove executable
   no-ops and check relocated targets and debug markers. Verify storage, loans,
   cleanup and the fixture bound call through the installed table. Preserve
   suspension and typed outcomes as interpreter coverage expands.
2. Add shared preparation, a scalar native entry ABI, and auditable publication.
   Bring up a scalar leaf on Winch and Cranelift, then direct calls. Require
   budget and frame handoffs before admitting loops or recursion. Expand to the
   supported macOS and Linux targets on AArch64 and x86-64.
3. Extend verified preparation with aggregate layouts, loans and cleanup,
   closure environments, installed SDK bindings, managed allocation and
   resumable operations. Admit a feature to a native tier once its verifier,
   adapter and recovery tests pass. Keep the interpreter complete for the
   required language surface.
4. Test all shipping entry points against the replacement before changing
   dispatch. Include the CLI, `Session`, REPL, debugger, constant evaluation,
   runtime-loaded modules and dynamic call adapters. Switch execution once
   the replacement passes, including native-disabled and recovery paths.

The first executable fixture calls an `i64` increment routine with 41 and
checks the literal result 42. A second input forces overflow at the callee's
addition. Run the fixture uninterrupted and in one-instruction slices and
compare the result, failure span, and logical fuel. On the two native paths,
require an actual adapter entry event. Test a forced budget handoff to the
verified interpreter. Corrupt one required map and verify rejection through
the normal publisher. Pause a compile job while its dependency version
changes and verify that the stale candidate is rejected before entry.

| Fixture | Independent assertion |
| --- | --- |
| Scalar control flow and direct calls | Expected values for both branches and a call chain. A budget exit resumes at the next logical instruction and preserves cumulative fuel. Checked integer overflow and zero division report distinct failures and exact source spans. |
| Non-GC loop and early exits | Peak live storage stays bounded as iteration count grows. Return, break, continue and failure retire each initialized value exactly once. Live payload borrows prevent reuse. |
| Closure and overloaded operator | Distinct closures at the same call site observe their own captures. Imports and shadowing preserve declaration identity. Inline and guarded paths preserve evaluated operands and effects. |
| SDK, GC and RC | The installed contract provides the expected result and effects. GC during construction retains the live roots. Allocation failure, cancellation and weak upgrade preserve the documented ownership and cleanup behavior. |
| Native publication and recovery | Reject malformed maps and relocations and stale dependencies before entry. Preserve active code pins. Force guard misses and budget exits and check exact logical state and a single host effect. |

| Execution entry | Cutover check |
| --- | --- |
| CLI, library Session and native executables | Execute verified code in all supported dialects and record native entry when required. Audit transitive runtime helpers. [Session run](../src/runtime.rs#L663) · [Current native step helper](../src/backend/abi.rs#L23) |
| REPL and debugger | Publish between submissions and preserve committed values after rejection. Step and resolve breakpoints at verified instruction origins. [REPL](../src/repl.rs) · [Debugger](../src/debugger.rs) |
| Constant evaluation during compile and check | Run the verified interpreter with isolated state, explicit limits and restricted capabilities. [Current constant execution](../src/constants.rs#L656) |
| Dynamic loading, initialization and generated adapters | Validate code for private initialization before exposing the initialized namespace. Verify exactly-once initialization, permissions and typed failures. Retain previous code versions for in-flight activations. [Runtime module publication](../src/runtime/modules.rs#L83) · [Generated dynamic calls](../src/dynamic_modules/callable.rs#L35) |
| Workers, generators, bound calls and cleanup | Run spawned and resumed activations through verified code. A pending operation consumes its recorded completion once. Cancellation and abrupt stop remain distinct. Check pins and roots across handoffs. [Worker entry](../src/runtime/concurrent.rs#L547) · [Concurrency contract](concurrency.md) |

Reference results come from the language and runtime contracts. The verified
interpreter becomes an executable reference for the native adapters after it
passes independent fixtures. Native agreement is additional evidence. The
direct cutover excludes historical execution from dispatch, including
fallback. [Verification and migration](compiler-runtime-overhaul-plan.md#verification-performance-and-migration)

## Source references for the design

| Reference inspected locally | Design implication |
| --- | --- |
| Zig `5e36170b`, `src/Air.zig:1` and `src/codegen.zig:149,184` | Function-local analyzed IR and separate generation and emission help keep target representation and linking out of semantic preparation. |
| V8 15.8.30.0, `src/compiler/backend/register-allocator.cc:4790`, `src/compiler/backend/code-generator.cc:613,1116`, and `src/deoptimizer/deoptimizer.cc:1700` | Allocation identifies live locations, emission binds them to machine PCs, and deoptimization translates saved state to logical output frames. Cyallo adds explicit ownership and cleanup phases. |
| Go 1.27.1, `src/runtime/stack.go:24,929,1212` and `src/runtime/asm_arm64.s:437,480` | Check frame space before entry, switch to the worker system stack for growth, and repair a saved coroutine context under exclusive access. Cyallo keeps host-pending inputs in owned or rooted storage. |
