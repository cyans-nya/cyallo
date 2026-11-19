# Winch and Cranelift implementation plan

The verified interpreter and its SDK execution fixtures come first. Both
native adapters will consume the same verified native plan. The runtime
validates and publishes each image before any native entry can execute.
Switch all shipping execution entry points together after the integrated
replacement passes the language contracts. [Backend design](backend-design.md) · [Direct cutover](compiler-runtime-overhaul-plan.md#direct-execution-cutover)

The scalar interpreter supports v7 bytecode, local loans and resumable
direct and borrowed calls.
Installed-table fixtures exercise a checked Cyallo scalar reference and a
synchronous host callback through aliases. The host fixture checks effects
and terminal failure across one-instruction slices. Nontrivial aggregate
cleanup remains open for interpreter conformance. [Interpreter tests](../tests/verified_bytecode.rs) · [Loan fixtures](../tests/verified_loans.rs) · [Source fixture](../tests/sdk_reference.rs) · [Host fixture](../tests/sdk_host.rs) · [Implementation stages](overhaul-stages.md)

## Review and acceptance

Have at least one independent subagent review each plan or design before its
implementation begins. The reviewer checks dependency direction, ownership
of ABI and layout facts, validation-before-entry, preservation of exactly-once
effects across exits, and traceability from source span through verified PC
to machine PC. Record findings and their disposition alongside the plan.
Each stage names the module owner, prerequisites, safe parallel work,
positive and negative tests, and its exit gate.

The independent backend conformance review identified gaps in scalar call
recovery, host operation ordering, intermediate feature gates, publication
races, and the cutover matrix. It also requested exact image obligations,
transitive pins, explicit dependency direction, and a linked source trace.
Stages 2, 5, 6, and 7 and the compile scheduling section incorporate those
findings. The follow-up review also clarified collection at non-parking
bridges and permit lifetime for running jobs that receive cancellation.
The unlinked emitter stages remain ahead of first native execution.

Expected behavior comes from Cyallo, runtime and SDK contracts. The verified
interpreter becomes the executable reference after its independent assertions
pass. Native tests also assert literal results, typed failures, source spans,
effect counts and resource bounds. Require direct evidence of a native entry
for any test that claims native coverage. Timing and code-size measurements
are recorded with workload and environment and inform later work.
[Quality rules](backend-design.md#code-quality-rules) · [Regression record](pending-decisions/07-baseline-failures.md)

## Interfaces and owners

| Module | Owns | Interface product |
| --- | --- | --- |
| Verified bytecode and interpreter | Canonical operations and cost, verified function and source identities, storage transitions, resumable activations | Immutable verified functions and typed return, failure, and handoff outcomes |
| Native preparation | Legalization, target layout, call ABI, stack object requirements, checked failure edges, and statepoint obligations | An owned prepared function for a particular target, with typed operation, layout, symbol, dependency and statepoint IDs |
| Winch | Fast instruction selection, assembler, conservative placement, prologue and fixups | Unlinked bytes, typed relocations, entries and final locations for the prepared obligations |
| Cranelift | CLIF translation, optimization profile, instruction selection, final code and locations | The same unlinked image contract as Winch |
| Code publisher | Structural validation against the exact prepared function, linking, executable mapping, install and retirement | A runtime-scoped executable entry whose pin retains code, dependencies and metadata |
| Runtime and compile admission | Execution policy, instance jobs, installed SDK bindings, feedback, scheduler transitions and code selection | A compile request with immutable facts and a typed decision to interpret, enter native code, or resume a verified continuation |

Use a whole-function emitter interface with an immutable prepared plan and
worker-local scratch. Extend the shared plan for each new language obligation.
Both emitters get named target or ABI layouts, checked offsets and sizes,
typed tags, and resolved binding IDs. Constrain unsafe executable memory,
pointer conversion, entry thunks and stack transitions to the platform and
runtime modules. Every unsafe block names the facts it relies on and where
they were checked. Review readable code, typed errors, deterministic order
and negative tests at the normal interface. [Backend interfaces](backend-design.md#compiler-and-runtime-interfaces)

Verified bytecode has no dependency on an engine. The interpreter and
preparation depend on verified bytecode. The two emitters and the code
publisher depend on preparation's plan and image contracts. The runtime
composes them and owns dispatch, admission and live SDK bindings. Keep
dependency arrows in this direction, including build features. Preparation
owns the guest context, closure environment, outcome and result-place
schema and classifies calls. Each adapter chooses final machine placement
and reports it. The publisher checks placement against the issued contract.

| Current coupling | Replacement |
| --- | --- |
| Cranelift embeds an inline cache width and reads a neighboring field using `offset + 8`. | An instance-owned feedback snapshot has a named capacity policy and typed slots. Shared preparation defines guard order and supplies typed entry projections from the target layout. [Current code](../src/backend/cranelift_backend.rs#L826) |
| `native_step(Session)` hides an entire VM instruction behind a generic call. | A bridge names one prepared runtime operation or one verified handoff. Its signature, effects, passing modes, destination and continuation are explicit. [Current helper](../src/backend/abi.rs#L54) |
| Winch compiles and installs its executable entry together. | Winch returns an unlinked image. The code publisher validates it, resolves relocations, installs it into the owning runtime and creates a pin. [Current Winch entry](../src/backend/winch.rs#L20) |

## Staged work

| JIT plan | Overall overhaul stages |
| --- | --- |
| 1 | 3–5 for verified bytecode, value semantics and the SDK reference harness |
| 2–5 | 6 for shared preparation, both native adapters and scalar publication |
| 6 | Remaining stage 6 feature coverage, stage 7 feedback and safe SDK emission, and stage 8 native suspension and OSR |
| 7 | Stage 10 direct cutover after the release dependencies from stage 9 are ready |

[Overall stages](overhaul-stages.md#stage-sequence)

## Implementation slices

Each slice has one independently testable result. A slice can start when its
listed dependencies pass. The stage gates below still govern native emission,
native execution, and the shipping cutover.

| Slice | Depends on | Exit check |
| --- | --- | --- |
| 1A. Scalar call results | v5 direct calls and synchronous SDK fixtures | A caller can discard a scalar result without allocating a destination. The callee still runs, charges fuel and reports failure. Verify direct and bound calls in one-instruction slices. |
| 1B. Shared scalar loans | v5 storage lifetimes and checked borrow provenance | A named local loan keeps its owner live through the last use and allows reuse afterward. Reject dead owners, incompatible merges and escaping loans. |
| 1C. Exclusive loans and borrowed calls | 1B | Verify conflicting accesses and preserve the caller's home across calls and fuel pauses. Reject conflicting arguments and unsupported escapes. |
| 1D. Cleanup at control-flow exits | 1C | Verify partial initialization, drops and bounded slot reuse on loops, return and failure. Resolve the pending value and lifetime decisions before widening coverage. |
| 2A. Shared scalar layout and ABI | Stage 1 gate | Prepare one inspectable scalar branch fixture with typed argument, result and layout facts. Reject unsupported sizes, projections and targets. |
| 2B. Prepared calls and recovery | 2A | Prepare checked failures and scalar call/budget recovery with exact PC, live value, return destination and fuel obligations. |
| 3A. Unlinked Winch | 2B | Inspect baseline prologues, branches, checked arithmetic and typed relocations for the common fixtures on the development target. |
| 4A. Unlinked Cranelift | 2B | In both profiles, inspect the common fixtures and verify final machine locations after register allocation. This work can proceed alongside 3A. |
| 5A. Validate and publish scalar images | 3A and 4A | Reject malformed or stale images. Pin versions and direct-call dependencies. Execute a linked scalar fixture and trace its span to the machine PC and back. |
| 5B. Mixed tiers and target matrix | 5A | Resume both logical frames after a budget exit inside a mixed-tier call. Run both adapters on the four supported platform combinations. |
| 6A. Values, calls and runtime bridges | 5B and per-feature interpreter conformance | Admit aggregate layouts and loans, then closure and indirect calls, managed bridges, and safe SDK emission as separately tested feature slices. |
| 6B. Feedback, compile admission and recovery | 5B. Each feature also needs its 6A gate. | Check publication races, guard misses, inline origins and effect phases. Use deterministic completion orders and independent recovery assertions. |
| 6C. Native suspension and stack relocation | 6A and 6B | Verify exactly-once parking, cancellation, GC roots, stack growth and worker migration before enabling retained native stacks. |
| 6D. Loop OSR | 6C | Import and resume the same invocation at a declared loop entry while preserving live values, ownership and fuel. |
| 7A. Direct cutover | Stages 1–6 and release dependencies | Run the required entry-point and platform matrix on the replacement, then switch shipping execution in one integration change. |

**1A verified.** Verified lowering omits storage for unused scalar call
results. Direct and bound calls still execute and propagate failure. Bound
calls validate the returned type before discarding the value, and void calls
still reject a destination. [Direct call tests](../tests/verified_bytecode.rs) · [Bound call tests](../tests/sdk_host.rs)

**1B verified.** Verified bytecode records the owner of a named local shared
scalar loan. Copies retain that owner, and lowering keeps it live through
the last use of every alias. Verification rejects early release, mutation,
inconsistent joins, and escaped loans. Interpreter slots can be reused after
the loan and owner lifetimes end. [Loan fixtures](../tests/verified_loans.rs)

**1C verified for scalar references.** Borrowed arguments preserve a logical
caller frame and register through nested calls and budget pauses. Verification
rejects conflicting arguments and direct access under an exclusive loan.
[Borrowed call fixtures](../tests/verified_loans.rs)

**1D scalar gate verified.** The verifier preserves conditional initialization
at joins and records storage for reachable PCs. Loop fixtures cover loan cleanup
and scope reuse on break, continue and backedges. Scalar failure releases the
activation's frame storage. Aggregate and enum payload loans, observable drops,
and the scope-release policy for the existing cell engine retain their separate
conformance gates. [Cleanup fixtures](../tests/verified_loans.rs) · [Pending lifetime decisions](pending-decisions/06-memory-lifetimes.md)

**2A and 2B verified.** Shared preparation classifies scalar ABI homes,
preserves versioned origins, and records per-PC fuel and initialized storage.
Each direct call has a typed target, projected arguments, destination and
successor. The first native subset admits scalar recovery with definite
initialization. Path-dependent initialization remains available to verified
interpretation. [Preparation fixtures](../tests/verified_native.rs)

**3A and 4A verified.** Winch and both Cranelift profiles emit the same
immutable plan and report final machine ranges after fixups. Cranelift uses
SSA homes and fuel across hot edges. It spills live values at cold budget
exits and call handoffs, with finalized CFG and trap metadata available for
inspection. Both emitters declare typed call address relocations in the
linkage table. This scalar ABI admits position-independent text and validates
call addresses before dispatch. [Emitter fixtures](../tests/verified_native.rs)

**5A verified for scalar code.** The code store checks exact plan identity,
generation, target, source and machine ranges, and relocation kind, bounds,
addend and target. It resolves and retains the callee ABI and transitive
entry pins before the platform maps executable memory. Linked Winch and
Cranelift fixtures execute checked scalar operations, report typed failures
and map linked machine addresses back to versioned bytecode PCs and spans.
[Publication](../src/backend/verified/publish.rs) · [Platform mapping](../src/backend/verified/platform.rs)

**5B verified for scalar recovery.** Winch-to-Cranelift and
Cranelift-to-Winch fixtures exhaust a slice in the callee and restore both
frames in verified execution. They check exact fuel, the caller's live values
and return destination, and the statement edge. A suspended caller retains
retired transitive dependencies. The focused suite covers macOS and Linux
on AArch64 and x86-64, with local x86-64 execution on an Apple Silicon host.
The CI matrix provides a separate runner for each target. Cyclic native call
graphs still require a link strategy and conformance. [Mixed-tier tests](../tests/verified_native.rs) · [Four-target CI matrix](../.github/workflows/native.yml)

### 1. Complete verified interpreter and SDK conformance

**Owner** — Verified interpreter and SDK-linked lowering.

**Dependencies** — Verified scalar activation, installed table linking and the
source and synchronous host fixtures. Settle the required operation semantics in
the pending decisions before admitting dependent features.

**Safe parallel work** — Prepare independent scalar, storage and SDK fixtures.
Audit the default execution, constants, loader and scheduler entry points.
Review target ABI and code-buffer interfaces from source.

**Tests** — One-instruction resume across calls and loops, overflow, source
spans, canonical fuel and frame limits. Exercise loans and slot reuse,
partial initialization, drops and early exits. Execute a bound call with a
controlled effect and verify one invocation across resume and failure.
Pair these cases with invalid storage transitions, invalid aliases or
ownership, missing bindings, signature mismatch and unsupported execution.

**Exit gate** — Independent fixtures pass through the replacement interpreter
with the scalar slice's required cleanup and binding contracts, including a
controlled host effect. Later language features need independent interpreter
coverage before their own native admission and shipping coverage before
cutover. Native emission begins after the plan review and this gate.

### 2. Establish typed layout, ABI and preparation

**Owner** — Shared native preparation.

**Dependencies** — Verified control flow, checked effects and liveness, and
the interpreter's failure, fuel and continuation contracts.

**Safe parallel work** — Specify unlinked image and statepoint schemas.
Build a fixture corpus at the prepared-function interface. Prepare publisher
validation cases and target-specific layout assertions.

**Tests** — Prepare scalar branches, direct calls and checked arithmetic.
Assert typed ABI argument and result locations, stack size and alignment,
correct source origins, and failure and pause edges. Reject out-of-range
projections, incompatible layouts and calling conventions, and unavailable
native features with typed errors. Confirm that bytecode handoff costs are
charged once, including lifetime markers. Check host-visible `repr(C)` ABI
size, alignment and field offsets against the shared layout on each target.
Prepare scalar recovery recipes that preserve pinned logical callers,
initialized scalar values, return destinations, next verified PCs and
fuel already debited across a nested call.

**Exit gate** — One privately constructible prepared function supplies every
ABI, layout and recovery obligation both emitters need for the initial scalar
slice. Verify dependency edges and source identities. Enforce named layout
facts and limits, typed IDs and tags, and checked offset and size conversions
through mechanical checks where possible. Review emitter code for unchecked
reconstruction of layout or call ABI. Deny
`unsafe_op_in_unsafe_fn` and enable Clippy's `undocumented_unsafe_blocks`
lint for the native crates. Apply the same checks to each adapter at its gate.

### 3. Emit the Winch scalar slice

**Owner** — Winch adapter.

**Dependencies** — Prepared functions and the unlinked image schema.

**Safe parallel work** — Implement independent validation tests and collect
instruction selection and final-location requirements for Cranelift. Keep
executable memory ownership with the publisher.

**Tests** — Emit straight-line scalar code and branches first. Inspect
prologues, checked add and divide, local branch labels, logical source
positions, argument and return locations, and typed relocations. Emit direct
calls with versioned symbols. Pair positive cases with unsupported target or
operation cases and missing required locations. Compare generated images
for identical immutable input. Use unlinked image and assembler inspection
tests in this stage.

**Exit gate** — Winch produces deterministic unlinked images and site
metadata for the prepared scalar fixtures on the development target.

### 4. Emit Cranelift for the same fixtures

**Owner** — Cranelift adapter.

**Dependencies** — The same plan and image schema and common fixture inputs.
The adapters can proceed in parallel after stage 2. Feed any common new
fact back to shared preparation.

**Safe parallel work** — Exercise platform ABI assertions and publisher
fault injection. Write mixed-emitter direct call fixtures with expected
arguments, returns and failures.

**Tests** — Translate the exact prepared scalar fixtures into both supported
Cranelift profiles. Check final machine locations after register allocation.
Spill required values to known homes where needed. Assert typed relocations,
source and machine PC bindings, and normalized deterministic metadata.
Pair each new image feature with a missing or invalid location case in the
image validator tests. Use unlinked image and CLIF inspection tests here.

**Exit gate** — Both adapters emit the same obligations and ABI for a
common plan. Each supplies the actual machine locations its emitted code
uses. Successful emission is an unlinked result.

### 5. Validate, publish and execute scalar code

**Owner** — Code publisher and runtime entry dispatcher.

**Dependencies** — Shared image contract, verifier and both emitters.
Before the first native execution, compare entries, exits, source maps and
statepoints against every obligation issued by the exact prepared plan.
Check the target, ABI, signatures, ranges, location widths and ownership,
and required writability. Check each relocation's kind, bounds, addend,
resolved target and versioned dependency. Validate the final linked image
and executable ranges before exposing any entry. Install executable
permissions and synchronize the instruction cache on the platform path.
An entry pin retains its code, metadata and transitive direct-call
dependencies. Entry selection and acquiring the pin form one runtime operation.

**Safe parallel work** — Bring up the same validator and entry fixtures on
the target matrix. Design the feedback and recovery extensions in parallel.

**Tests** — Assert scalar result, overflow, division by zero, source span,
and exact budget handoff in both adapters. Require the requested adapter's
entry event. Exhaust a slice inside a mixed-tier callee in each direction.
Resume both frames in verified interpretation and check the return value,
successor PCs and fuel against independent expectations. Retire a direct
callee while a suspended caller still pins it and show that both code and
metadata remain accessible until the pin is released. Require typed errors
for incompatible targets, corrupt relocations, missing and invalid plan
obligations, stale generations and foreign runtime identity. A failed
installation publishes no callable entry. Test native-disabled and
native-ineligible requests against verified interpretation.

Trace a linked fixture from its source span to a versioned verified PC,
prepared origin and required statepoint, and the actual linked machine PC
after register allocation and fixups. From that PC, recover the expected
source failure or verified continuation. Check both adapters and require
fuel and frame-limit tests before admitting loops and recursive calls.

**Exit gate** — Only a validated, pinned image is callable. Scalar fixtures
run through both adapters and resume in the verified interpreter at
explicit handoff sites. Expand and record coverage for macOS and Linux on
AArch64 and x86-64. Advanced statepoints have separate admission gates.

### 6. Add feedback, recovery, suspension and OSR

**Owner** — Runtime feedback and recovery with shared preparation. Each
adapter binds the common recovery obligations to final machine locations.

**Dependencies** — Validated scalar publication, pinned logical bytecode,
explicit runtime outcomes, and verified continuations. Extend the
interpreter contracts before admitting each new native feature.

**Safe parallel work** — Collect bounded feedback and test entry guards while
designing root, relocation and cleanup maps. Bring up target-specific
stack transition thunks against independent relocation fixtures.

**Tests and internal gates**

1. Layer in feature slices. For each slice, implement preparation and
   adapter support, verify with independent assertions and map validation,
   then enable native admission for that slice. Until its gate passes,
   execute the feature in the verified interpreter.

   | Slice | Verification before admission |
   | --- | --- |
   | Stack objects, aggregates, enums and loans | Assert generic shape layouts, bounded reuse, partial initialization, cleanup order, and stable homes for live loans on success and early exit. |
   | Closure, trait and indirect calls | Assert closure environment identity, repeatable/exclusive/consuming calls, resolved overloads and cleanup on failure. |
   | Managed storage and typed runtime bridges | Assert explicit allocation and ownership operations, GC root maps, weak owner behavior, allocation failure, and cleanup across exits. Bridge effects and commit phases come from the checked descriptor. Before any collecting or reentrant bridge call, publish live roots. After the call, reload traced values and reconstruct derived references from their updated bases. Force compaction in a non-parking bridge fixture and assert that the caller reads through the updated references. |
   | Installed SDK bindings and safe emitter | Add a binding with a checked Cyallo or declarative reference. Require the same values, failures and effect counts on both emitters and the interpreter. Reject stale dependencies and invalid emitter output before publication. |

2. Capture advisory call-site feedback by function instance and resolved
   site. Guard code, layout and binding versions before an effect or
   ownership transfer. A guard miss reuses evaluated operands and the actual
   closure environment. Recheck dependencies at publication.
3. Add pinned recovery recipes for later exits. Reconstruct logical and
   inlined frames, exact next instructions, initialized values, roots and
   cleanup ownership. Test an exit before and after an effect commits.
   A resumed pending operation delivers its result once. Reject missing
   recipes and recipes that could repeat an effect or lose an owner.
   Extend the linked source trace to inlined origins and logical callers.
4. Gate any bridge that can park on the scoped pending-operation protocol.
   Register an operation ID, result destination and verified successor
   before host submission. Retain owned buffers or rooted descriptors
   across Pending. The runtime arbitrates cancellation and completion for
   a single outcome and ownership commit. Release the processor lease
   while parked and reacquire one before resuming. Keep cancellation
   cleanup separate from abrupt stop. Check early and duplicate completion,
   cancellation that wins before commit, and deoptimization after commit.
   Check that each path reaches its successor with the correct effect
   count, cleanup phase and processor budget.
5. Admit native suspension and stack copying after the required feature,
   recovery and bridge gates, with complete writable relocation maps, saved
   context, roots and cleanup phases. Reserve space before entry, use the
   worker system stack for host and Rust calls, and move guest stack state
   under exclusive ownership. Test GC, cancellation, live-borrow repair,
   growth failure and worker migration. Reject images with unsupported
   moving references and incomplete maps.
6. Add loop OSR at a declared verified loop entry. Verify version, live
   values, ownership and stable homes under loans before importing a frame.
   Test failed imports and exits from the same invocation, including fuel
   handoff and borrowed homes across loops.

**Exit gate** — Each admitted feature has independent tests for values,
effects, ownership, cancellation and recovery on all enabled targets.
Missing maps or unsupported states prevent native admission.

### 7. Direct cutover

**Owner** — Release integration for compiler, runtime, tooling and SDK.

**Dependencies** — Required verified interpreter coverage and native
conformance on the supported target matrix. Integrate under the selected
repository and SDK policies.

**Safe parallel work** — Validate CLI, library Session, REPL, debugger,
constant evaluation, dynamic load, worker, generator and bound-call entry
points on the integrated candidate. Record representative measurements.

**Tests** — Check required language outputs, diagnostics and resource
bounds through the replacement. Exercise native-disabled, ineligible,
stale, guard-miss and debugger paths. Confirm each uses verified execution.
For suspension, ownership and GC, cover eager and deferred checking,
verified interpretation, each admitted native statepoint, moving GC stress
and one-instruction slices. Require native entry and handoff evidence on
macOS and Linux on AArch64 and x86-64 for enabled features. Audit transitive
library, dynamic and generated entry points for the legacy decoder, generic
`native_step` bridges, and canonical instruction and continuation identity.

**Exit gate** — One shipping integration change routes execution through
the verified interpreter or a validated native image and removes the
legacy execution route. The same contract fixtures govern acceptance on
each platform. [Cutover inventory](backend-design.md#first-implementation-slice-and-cutover-tests)

## Tier admission and compile scheduling

- **Entry selection.** The dispatcher can select a published pinned entry.
  Verified interpretation owns the generic path and can request compilation
  for a later invocation. Winch is the baseline tier.
  The optimized Cranelift profile is admitted from observed hotness or an
  approved compile hint. The low-latency Cranelift profile remains an
  explicit policy selection. Use named thresholds and bounded compile work
  in the runtime policy. Honor existing compile hints and verify whether
  entry and statepoint kinds are supported ahead of codegen.
- **Request identity** includes instance, function instance, requested tier
  and target, program/code generation, ABI and dependency versions. Coalesce
  repeated requests. An upgrade advances the slot's request revision and
  sets its desired tier. A superseded result is ineligible in either
  completion order, even within the same program generation. An already
  published compatible entry remains callable while the upgrade is pending.
- **Capacity and fairness.** Runtime instances own their jobs, immutable
  snapshots and result mailboxes. An explicitly shared compile admission
  capability issued by the runtime factory owns the ready-instance queue
  and round-robin cursor for its participating runtimes. Independent
  capabilities have independent queues and share worker capacity. Each
  instance has named queue and in-flight caps. Skip capped or empty
  instances without reserving work for them. Each started job owns a permit
  released once the job actually stops occupying capacity, including when
  it observes a cancellation request and exits. The process-wide pool
  supplies worker capacity. Idle workers retain no job or instance state.
- **Cancellation and publication.** Dropping or resetting an instance
  invalidates its generation and cancels queued requests. Workers poll the
  supplied cancellation token at named stages. A late result is discarded
  by the owning mailbox or rejected at installation. Serialize cancellation,
  revision/tier and version checks, dependency checks and install with
  entry selection and pin acquisition in the owning runtime. Active and
  suspended activations retain their pins.
- **Deterministic tests.** Use a controlled worker and completion order to
  exercise coalescing, cap exhaustion, two-instance round-robin service,
  cancellation before and during compile and between validation and install,
  both completion orders for a superseded job, and stale-result rejection.
  Hold a cancelled job on a worker and assert it still counts against the
  in-flight cap until the worker finishes that job.
  For identical verified inputs compare emitted bytes and symbolic metadata,
  while checking that published entries retain their own runtime identity.
