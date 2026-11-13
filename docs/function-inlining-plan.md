# Routine inlining and related optimization plan

Status: the bounded initial implementation is complete: strict purity, bounded
compile-time constants with immutable pools, frameless scalar routine inlining,
opt-in embedded procedure bodies with retained frames, exact accounting and matched
on/off benchmarks. See [results and supported limits](routine-optimization-results.md).
General speculative dynamic inlining, captured callable constants and broader
CSE/hoisting remain future work under the gates below. This builds on the
[JIT remediation](jit-remediation-results.md), without changing its tier
thresholds or eager-engine dispatch layout.

The first target is a hot scalar call chain such as `main -> cube -> square`.
Compile eligible callee bodies into the caller's native SSA values, eliminating
VM frame setup, argument/result boxing, native entry dispatch and return helpers
on successful inline executions. The scope also includes procedures, with a
separate frame-preserving path for effectful bodies. Keep canonical bytecode and
ordinary execution available for interpretation, suspension and failures.

**Agreed constant contract and pending binding syntax.**

- `fn` means strict value-level purity: the same logical arguments and immutable
  dependencies, under the same semantic body revision, produce the same result
  without observable side effects. Reads of mutable external state through a
  global, shared handle or capture do not satisfy this rule. GC relocation does
  not change a logical argument or value.
- Keep the spelling `const`: a compile-time evaluated, deeply immutable logical
  value, automatically substituted at uses. This is stronger than a runtime
  binding that merely cannot be reassigned.
- Literals, constant expressions and other constants are allowed. If initialization
  calls a user routine, it must be a modern pure `fn`, never an executing `proc`.
  Builtins require an explicit deterministic, compile-time-safe whitelist.
- Pure is necessary for such a call, not sufficient for compile-time evaluation:
  arguments and dependencies must be compile-time known; effects must be checked through
  the whole called graph; no runtime input, mutable managed storage or random
  source is available. A pure call with a runtime argument is not a constant.
- Allow private evaluator-local mutation/allocation where supported, then freeze
  the returned value graph. Apply work, recursion and allocation limits; checked
  failures or exceeded evaluation limits produce compile-time diagnostics.
  Pure but nonterminating routines must not hang compilation. A separate
  `const fn` annotation is not required initially; eligibility is checked.
- Scalars can become native immediates. Larger immutable values use an immutable
  representation or constant pool rather than duplicating their payload in every
  instruction. Session-managed instances must remain Session-owned and GC-rooted.
- Substituting a constant value does not imply a stable address. Managed handles
  and cell indices must be repaired or reloaded across compaction; generated code
  must not retain an untracked raw pointer across a collecting operation.
- Mutable copies of constant aggregates must copy or detach before writing.
  Shared mutable storage, borrowed mutable locations and mutable captures cannot
  be smuggled into a deeply immutable constant graph.
- A constant callable value and its semantic body revision are separate facts.
  Constant substitution does not authorize executing stale inlined code after a
  semantic routine replacement, nor force unlimited body inlining.
- Runtime `let` is immutable and `let mut` permits mutation. A read-only
  binding does not prove that referenced interior-mutable storage is immutable.

Implementation: modern constants pass through checked, bounded evaluation and
deep freezing before final lowering. Scalar uses become immediates; aggregate
payloads share immutable pool entries. The legacy compatibility `CONSTANT` path
retains its literal restriction. Compile-time value facts and pool payloads are
separate from the binding's `Local.constant` write protection and runtime versions.

Regression baseline: `const value = rand(1)` was accepted and emitted a runtime
builtin call. It now produces a compile-time diagnostic. `const n = square(12)`
is evaluated during compilation; executing a procedure or using
`square(runtime_input)` as an initializer is rejected.
Storing a stateless procedure reference as an immutable value is a separate
case from executing that procedure during initialization; holding the reference
does not make a later call pure.

**Initial scope and limits.**

| Item | Initial decision |
| --- | --- |
| Backends | Cranelift fast and speed; identical inlining policy |
| Frameless primitive types | INTEGER and BOOLEAN arguments/locals/results; VOID procedure results also supported |
| Frameless storage | BYVAL parameters and register-resident locals only |
| Initial control flow | Bounded straight-line bodies through one return |
| Frameless operations | Literals, moves, frame loads/stores, checked integer add/subtract/multiply/negation, boolean NOT and primitive comparisons |
| Procedure effects | Frame-preserving body inlining; retain ordinary helper, alias and GC contracts |
| Initial calls | Direct `Op::Call` within an immutable Program revision |
| Callee size | At most 32 reachable bytecodes per callee |
| Expansion | At most 64 expanded callee bytecodes per call site |
| Nesting | At most three inlined callee levels; reject recursive cycles |
| Caller growth | At most 256 expanded callee bytecodes per compiled caller |

The frameless path excludes branches/loops, recursion, BYREF/escaping locations,
managed accesses, closures, dynamic/trait/service calls, builtins, I/O,
division/modulo and floating-point operations initially. Effectful procedures
use the frame-preserving path below; unsupported regions remain out of line.
Routine kind alone does not decide eligibility. Ineligible code remains valid.

These limits bound code growth. The frameless subset additionally has a known
complete execution cost and depth; frame-preserving procedures can suspend at
ordinary bytecode boundaries. Extend control flow and operators after the first
implementation passes its gates.

**Validity, invalidation and deoptimization contract.**

1. **Distinguish semantic identity from machine code.** Record the Program
   generation and semantic routine/body identity for every copied body, including
   nested callees and folded constants. The current owned `Arc<Program>` plus
   routine ID identifies an immutable snapshot. Native `code_version`, an entry
   address, and the scheduler's request `generation` are not semantic revisions.
   A tier upgrade with an unchanged body does not invalidate an inline copy.
2. **Forbid unversioned edits to published code.** Current native entries use
   Program identity checks, and the REPL publishes a new Program and remaps
   references. Preserve that model initially. Lazy checking/lowering of the same
   frozen body is not a semantic edit. In-place bytecode changes under an unchanged
   Program identity would bypass today's guard and must not be introduced.
3. **Keep dynamic calls dynamic until guarded.** Initial planning declines
   `Op::Invoke` and dynamically resolved trait/service targets. Reassigning a
   function value must resolve the new target on the next call. Later speculative
   inlining must guard the current binding/target identity, body revision and
   signature/layout, plus captures, receiver or binding lifetime as applicable.
   A matching code pointer or function ID alone is insufficient.
4. **Miss before committing.** If a target/version guard fails before the region,
   save canonical caller state at the original call PC and execute the actual
   current target. Do not call the remembered target, reevaluate argument
   expressions, double-charge work, or replay effectful target resolution.
5. **Invalidate all semantic dependents.** When semantic replacement is supported,
   maintain callee/constant-to-compiled-caller dependencies, including transitive
   inline trees. Publish new immutable revisions at execution safepoints, prevent
   stale callers from entering, and reject compiler results whose dependencies
   changed while compilation ran. Existing request-ticket generations alone do
   not provide this check. Invalidation is mandatory; recompilation can be lazy.
6. **Define active-call behavior.** Initial bounded frameless regions contain no
   callbacks, collecting operations or mutation points and complete against a
   pinned snapshot. Do not permit asynchronous in-place changes while they run.
   If live replacement later affects active invocations, either let those
   invocations finish on pinned old revisions or deopt at a mapped safepoint.
   Never resume an old PC against unrelated new bytecode.
7. **Require maps for general mid-region deopt.** Maps must recover logical inline
   frames, source/body revisions, bytecode PCs, locals/registers and initialization
   state, return destinations, roots/aliases, and already charged work/effects.
   Resume the interpreter/lower tier at the correct point without repeating an
   effect. Until those maps exist, do not admit a speculative region that needs
   them. Retained real procedure frames provide the first safe alternative.
8. **Retire safely.** Pin old body snapshots and executable owners for active
   calls, suspended frames and applicable continuations. Remove future entry
   eligibility immediately on invalidation; reclaim only after executing and
   suspended users no longer need the version. Moving GC changes physical
   locations, not the semantic value/body revision of an immutable constant.

Today's Program check rejects stale artifacts; it is not transparent deoptimization.
The driver must choose a current artifact or interpretation after replacement.
Do not advertise mutable-body/dynamic speculative inlining before these gates
and the corresponding side-exit behavior are implemented and tested.

**1. Establish the contract and the feedback loop.**

Primary seams: `tests/native_storage.rs`, `tests/tiering.rs`, `tests/metrics.rs`,
`tests/deferred.rs`, and a new focused `tests/inlining.rs` if grouping helps.

- Add an explicit inlining policy to `CraneliftConfig`, initially disabled, so
  the same artifact configuration can be compared with inlining on and off.
  Expose effective policy/limits in compilation metadata.
- Add lightweight per-Session counters for completed inlined calls and inlined
  work. They must be available without enabling full execution observation.
  Use them to assert an eligible call actually takes the fast path; output
  parity alone would also pass with no implementation.
- Preserve the pre-inlining release binary and benchmark `modern.ya`, the
  minimal square/cube workload, direct leaves, a flat loop without calls,
  recursion, arrays/strings, and 100/500-routine compilation workloads.
- Add differential fixtures that compare the interpreter and inlining-enabled
  native execution at budget exits, including per-function work and call counts.
- Include procedure writes and partial-effect failures, dynamic rebinding,
  semantic replacement and constants surviving collection in the baseline.

Acceptance: an eligible-call regression fails because no call is inlined, while
the ordinary execution path remains the reference. No timing assertions or
sleeps in correctness tests.

**2. Build a bounded, read-only inline planner.**

Add a small planning module, preferably `src/backend/inlining.rs`. Keep candidate
analysis separate from Cranelift instruction emission.

- Read checked typed IR, canonical bytecode PCs and `LocalStorage` placement.
  Frameless eligibility must prove register-only storage and the instruction
  allowlist; frame-preserving procedures retain existing storage/GC metadata.
  Strict purity alone does not prove eligibility for frame elimination: a pure
  routine can use private allocations, fail deterministically or diverge.
  Managed data reads are admissible as pure only when their logical values are
  immutable inputs/dependencies. Existing allocation effects are conservative
  and can describe locals before storage placement.
- For each eligible call site, produce parameter/register bindings, primitive
  types, reachable instructions, result binding, total work, per-function work
  and invocation contributions, maximum additional logical depth and semantic
  dependencies. Classify it as frameless, frame-preserving or out of line.
- Walk through the first reachable return; do not charge an unreachable tail.
  Count original call/return and statement bytecodes, not generated native
  instructions. Nested calls belong to their original logical routines.
- Enforce per-callee, per-site, nesting and per-caller growth limits. Cycle
  detection and deterministic source-order selection keep planning bounded.
- Under automatic JIT, only inspect already-analyzed callees. A missing or
  failed body declines inlining; it must not force checking an unused cold
  branch or fail caller compilation. Cache readiness decisions only within
  that compilation; later caller versions may discover more candidates.
- Use complete effect summaries where required. Treat deferred/incomplete
  transitive summaries as unknown, not as proof that a call is freely movable.

Acceptance: `square` is frameless-eligible; effectful procedures are classified
for frame-preserving emission. Unsupported instructions, recursion, unready
bodies and expansion limits produce explicit decline reasons. Planning
does not mutate Program IR, bytecode, signatures or runtime state.

**3. Emit a guarded scalar fast path with the original call as fallback.**

Primary implementation: `src/backend/cranelift_backend.rs`. Keep scalar-bytecode
lowering distinct from a new `try_inline_call` path.

- Generate SSA bindings for the callee's scalar registers instead of allocating
  VM frames or reading/writing callee register arrays. Frame moves become SSA
  assignments; checked arithmetic exposes value flow to the optimizer. BYVAL
  parameter mutation updates the callee's SSA binding, not the caller's argument.
- Intercept eligible calls alongside the existing call lowering, before its
  conservative GC-effect gate. Do not weaken canonical `Op::Call` safepoint
  metadata: the fast branch is noncollecting, while the fallback still publishes
  roots and performs all ordinary collecting/reentrant setup.
- Before entering the region, require initialized arguments of supported types,
  enough fuel to complete the entire expanded call, sufficient logical frame
  capacity, and an execution mode that permits inlining.
- Use nontrapping checked arithmetic to accumulate a failure condition. Do not
  publish a result, charge work or increment calls until the fast path succeeds.
  If any operation fails, execute the original call with its original fuel and
  state. The ordinary callee then produces the correct error, stack and span.
- Preserve argument evaluation and integer overflow semantics. For example,
  `cube(n)` becomes `checked_mul(checked_mul(n, n), n)`, with no reassociation
  or replacement by wrapping arithmetic.
- On success, commit the result and accounting, then jump directly to the
  caller's next bytecode PC. No runtime call or collecting operation is allowed
  inside the successful inline region.

Acceptance: a large-budget scalar call avoids frame creation and call/return
helpers; overflow, uninitialized arguments, insufficient fuel and frame-limit
cases use the original path. Speculative evaluation never causes a hardware
trap or changes externally visible state before commit.

**4. Integrate accounting, tiering and inspection before enabling it.**

Primary files: `src/runtime.rs`, `src/runtime/native.rs`, `src/metrics.rs`,
`src/backend/tiered.rs`, and the native emitter's polling logic.

- Preserve exact global instruction counts, per-function instruction counts
  and per-function call counts. Callee work must not be charged to the caller.
  Keep the existing saturating-counter behavior.
- Proposed native contract: expose refreshed pointers to the Session's work
  and call counters, plus a separate aggregate pending-inlined-work count in
  `NativeView`. Commit per-callee contributions on success; `finish_native`
  adds aggregate inline work to the global count separately from caller work.
  Refresh borrowed pointers at native preparation and program replacement.
- Audit all clean-state tests, native return synchronization and GC preparation
  for the new pending-work category. Inline work must be committed before any
  helper can observe Session counters or collect.
- Readiness polling uses total pending executed work. Per-routine promotion
  limits still use that routine's own work. Guard the inline tree's callee
  limits and fall back to normal entries when promotion needs reconsideration;
  inlining must not recreate the missed-promotion bug.
- Enforce `max_frames` using actual caller depth plus the plan's maximum logical
  depth. A failed guard falls back rather than reporting an early synthetic
  error; the ordinary call chain identifies the correct failing call site.
- For the first release, full execution reporting forces ordinary calls so
  invocation events, timing attribution, peak depth and inspected locals remain
  faithful. State this mode explicitly in reports. The source debugger retains
  its existing interpreter stepping. Lightweight optimization counters measure
  the unobserved fast path; report timings cannot stand in for that path.

Acceptance: sweeping budgets around the complete-call boundary, and enabling
reporting between native runs, preserves status, PC/span, stack, variables,
globals, function calls and instruction counts. Promotion requests remain
bounded by native safepoint spacing, independently of outer execution budgets.

**5. Extend frameless leaf inlining to short acyclic call chains.**

- First land the leaf path with complete accounting and fallback semantics.
  Then compose the same planner/emitter recursively within the stated limits.
- Inline `square` into `cube`, then the resulting scalar tree into its caller.
  Track repeated calls to the same function separately before aggregating work
  and invocation contributions.
- Keep the whole region side-effect-free until commit. If a nested operation
  fails, fall back to the original outer call so original evaluation order,
  partial progress, error stack and instruction consumption are reproduced.
- Retain independent out-of-line units. Machine-tier replacement is harmless
  only while the semantic body, signature and storage assumptions are unchanged.
  New Program/body revisions follow the invalidation contract above.
- Becoming ready without changing semantics is only a new optimization
  opportunity: that caller version may keep its ordinary call until a later
  compilation. This optional recompilation rule does not waive mandatory
  invalidation on semantic changes.

Acceptance: `main -> cube -> square` takes the inline path with sufficient fuel,
yet matches out-of-line execution at every tested handoff. Recursive and larger
call graphs remain out of line without excessive planning or code growth.

**6. Inline procedures with an effect-aware execution path.**

- Admit small noncollecting procedures with only private scalar effects to the
  frameless path, including `return` without a value. Use the same fuel, depth,
  error and accounting requirements as functions.
- For observable effects, first retain the real VM procedure frame and ordinary
  call setup. Inline its body into the caller's native code to remove the extra
  native entry/prologue/dispatch, while preserving source-level frames, parameter
  coercion, BYREF aliases, service leases and resource limits. This is body
  inlining, not a promise to eliminate the procedure's frame allocation yet.
- Give each inline instance a source routine/PC map and return continuation.
  Existing lowering currently assumes one owning routine; introduce explicit
  emission context rather than interpreting callee registers as caller registers.
- Before collecting/reentrant helpers, publish the procedure's canonical state
  and roots; reload relocated data afterward. Kill cached facts at writes,
  unknown calls and aliasing effects unless stronger analysis proves them safe.
- On budget exhaustion, input suspension, errors or invalidation, resume from
  the current canonical procedure PC. Never restart its call after a write, print,
  allocation or other effect has happened. Only the real return transfers control
  to the caller continuation, and each effect/return is charged exactly once.
- Keep recursive expansion bounded; arbitrary dynamic calls inside the copied
  body still use ordinary dispatch and can side-exit into the VM. Retain frames
  whenever deopt/root/alias requirements cannot be satisfied without them.

Acceptance: global and BYREF mutations, overlapping aliases, I/O before a failing
instruction, allocating helpers, nested calls and one-instruction suspensions
match interpretation. Procedure inlining is part of completion, not excluded
because a procedure is effectful. Frameless effectful-procedure optimization
waits for proper virtual-frame/deopt maps rather than speculative replay.

**7. Audit and stage other optimizations.**

First enforce the agreed constant contract in the modern front end: reject
runtime-dependent or mutable initializers, use bounded compile-time evaluation
with deterministic diagnostics, and preserve immutable aggregate/callable
identity where observable. Extend beyond existing constant expressions in
reviewable increments. Constant evaluation needs its own work/allocation limits;
pure recursive code must not hang compilation.

Enforce the intended strict purity contract before using it for optimization.
The baseline allowed readonly managed-storage access in modern `fn`;
the implementation now rejects it and migrates state-reading lookup helpers to
procedures with readonly validation at their call boundary. `reads_managed`
continues to record those dependencies. A function reading changing
external storage is readonly, but is not a mathematically pure function of its
declared arguments. This is a current semantic gap, not an exception to the
intended definition of purity.

Audit these functions, including managed lookup mappers. Prove their reads refer
to immutable values, pass an immutable snapshot/state explicitly as an argument,
or move the state-reading operation to `proc`/an explicitly distinguished readonly
operation. Private mutation of owned value copies remains permitted when it does
not escape as observable mutation. Update effect checking, declarations,
diagnostics, existing programs/tests and documentation together; do not simply
relabel a mutable read as constant because its handle binding is immutable.

Strict purity supports result reasoning; compile-time evaluability additionally
requires compile-time inputs, supported operations and evaluator resource bounds.
Do not equate deterministic behavior with guaranteed termination, successful
checked arithmetic or a compiler's ability to evaluate every call.

| Optimization | Required proof / constraint |
| --- | --- |
| Constant substitution and folding | Compile-time immutable value; scalar immediate or rooted pool reference; semantic dependencies pinned |
| Local SSA copy propagation and redundant load elimination | Def-use, initialization and alias facts; invalidate at relevant writes/helpers |
| Pure-call specialization / common expressions | Stable body and immutable inputs/read set; preserve failures, allocations, termination, depth limits and logical work/calls |
| Loop-invariant computation | Invariant operands/dependencies and legal evaluation order; no moving failing/allocating work into a zero-trip path |
| Devirtualization / guarded dynamic inlining | Actual target, semantic revision, call shape, captures/receiver and lifetime guards; valid side exit |
| Bounds/check elimination | Proven index range and initialized immutable shape; retain source-visible failure behavior where not proved |
| Procedure store/load forwarding | Alias and effect analysis; never reorder observable writes/I/O or erase an unknown call |
| Allocation elimination / unused-call removal | Additional identity, lifetime, resource and failure proofs; not justified by `fn` purity alone |

Prioritize constant substitution, SSA value propagation and bounded routine
inlining. Add broader CSE/hoisting only when the proofs and virtual accounting
exist. Existing Cranelift optimizations can consume the exposed SSA; independent
language-level rewrites must not erase source-visible errors or fuel costs.
Runtime expression folding must not raise an error from a path that never runs;
compile-time constant declarations use their explicitly compile-time error rules.

GC compaction is not semantic invalidation. It does require repairing roots,
handles and cached physical locations at collecting operations. Do not embed a
managed constant's current cell index/address as an eternally valid immediate.

**8. Benchmark, select defaults and document the supported subset.**

Extend `benches/engines.rs` with explicit on/off comparisons while holding
optimization level, allocation, scope, resume strategy and tier policy fixed.
Measure fast and speed independently, cold CLI latency, isolated compilation,
warm execution, emitted bytes, actual inlined call counts and retained/elided
procedure frames. Keep observation disabled for performance measurements.

Use three batches of at least 40 cold runs and rotate warm sample order. The
square/cube cases must demonstrate an execution improvement and elimination of
their successful call/return helpers. Procedure cases must prove exactly-once
effects and a measured dispatch/body-inlining benefit; label retained-frame
costs honestly. Investigate any greater than 5% median
or p95 regression repeating across three batches, especially flat loops and
large compilation workloads. Static expansion limits must be tested directly;
compile-time and code-size measurements remain separate from runtime claims.

Enable the same bounded policy for both eager Cranelift defaults and automatic
Cranelift tiers only after the gates pass. Winch stays a cheap out-of-line
baseline in this iteration. Keep an explicit disable setting for comparison
and rollback; report effective limits and rejection reasons in opt-in compiler
diagnostics. Do not promise a speedup for ineligible or error-heavy workloads.

**Required regression coverage and completion.**

- Integer extrema, checked overflow order, comparisons, uninitialized values,
  private/BYVAL mutation, repeated/nested calls, multiple call sites, unused
  results and rejected types.
- Budgets of one, just below/equal/above full inline cost, every partial-call
  handoff in the reference path, and caller/callee frame-limit boundaries.
- Logical work/call counts and tier requests with mixed interpreted/native
  callers, missing callee code, pending upgrades and reporting enabled/disabled.
- GC stress around eligible scalar calls and ineligible allocating/BYREF calls;
  input suspension and failures on fallback paths; deferred invalid cold bodies.
- Program replacement, both eager scopes/resume strategies, and compiled callers
  whose callee tier changes while their inline copy remains installed.
- Dynamic function rebinding between calls; same routine ID with a changed body
  revision; closure capture/receiver changes; stale compiler completions; pinned
  old revisions across suspension. Tier-only changes must not cause semantic
  invalidation. Tests must distinguish rejection of stale code from deoptimization.
- Procedure effects before a failure, input wait, aliasing write or invalidation;
  verify that no completed effect is replayed and that return executes once.
- Constant compile-time failures and evaluator limits, rejected mutable alias
  graphs, mutable copies of constants, constant-pool roots through compaction,
  and constant-dependent code invalidation on a new semantic generation.
- Constant calls to eligible pure functions versus procedures, random/input
  builtins, runtime arguments, mutable managed reads, transitive hidden effects,
  recursion exhaustion and supported private compile-time scratch allocation.
- Strict-purity checks for mutable globals, shared managed handles and captures;
  immutable snapshots remain valid inputs. Change external storage between two
  calls with otherwise equal arguments to expose an incorrectly admitted readonly
  dependency, rather than relying only on the existing `pure` flag.

Run the workspace tests, targeted release runtime suites, formatting checks and
the engine benchmark matrix. Completion requires observed use of the inline
fast path, frame-preserving procedure inlining, interpreter parity at handoffs,
preserved resource/error behavior, semantic dependency validity, bounded code
growth and published measurements. General control-flow/frameless effectful
inlining and speculative dynamic targets remain gated on the required deopt
maps and invalidation machinery; they must not be silently admitted. A new
fast-call ABI is a separate optimization after these measurements.
