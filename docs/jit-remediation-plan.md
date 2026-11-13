# JIT remediation plan

Implemented and validated; see [results and the final layout decision](jit-remediation-results.md).
The original draft below records the intended work and acceptance criteria.
Measurements selected a shared per-routine all-PC eager path after call
synchronization was fixed, avoiding the combined candidate's compiler-memory
growth. Admission thresholds were reviewed and retained after calibration.

Deliver correct tier requests at native safepoints, make engine comparisons
explicit about their execution structure, and reduce avoidable native-call
synchronization. Evaluate compilation latency and execution speed separately.
An optimizing engine can legitimately take longer on a short cold run.

Implement the work as the following ordered, separately reviewable changes.

**1. Preserve the reproduction and measurement baseline.**

- Turn the deterministic Winch prewarm probe into an integration regression in
  `tests/tiering.rs`: run a loop past the nonzero fast threshold in one large
  execution budget and assert that fast compilation was requested. Assert on
  the request and its recorded work count, rather than worker completion time.
- Add coverage for a repeatedly called leaf and a nested native callee. Both
  must cross their own work thresholds while a native caller remains active.
  Include routines that are ineligible for speed but still eligible for fast.
- Exercise large and small outer budgets, with Session reporting both enabled
  and disabled. Use compiler events to inspect request work counts without
  enabling expensive execution observation.
- Preserve `modern.ya`, a minimal square/cube workload, and a flat scalar
  loop as benchmark cases. Add cold automatic-JIT measurements alongside the
  existing fixed-tier warm measurements; cold cases must not reuse an already
  compiled artifact.

Acceptance: the new native threshold regressions fail on the current code and
identify the missed request. Record the current cold and warm measurements
before changing production behavior. Avoid sleeps and timing assertions in
correctness tests.

**2. Fix native tier-transition notification.**

Primary files: `src/backend/tiered.rs`, `src/runtime/native.rs`,
`src/backend/winch/emit.rs`, and `src/backend/cranelift_backend.rs`.

- Replace the speed-only interpretation of `hot_limits` with a per-routine
  next-transition limit. A routine running Winch needs the fast threshold;
  one already requesting/running fast needs the speed threshold only if speed
  is eligible. Otherwise its limit is disabled.
- Derive that limit from the highest desired, queued/running, and installed
  tiers. Once an upgrade is requested, stop yielding repeatedly for its old
  threshold while lower-tier code continues to execute.
- Handle failed requests and rejected speed candidates explicitly so a
  permanently unsatisfied limit cannot create a no-progress yield loop.
- Use the same limit contract in Winch and Cranelift polls, including native
  callee entry. After a callee triggers a yield, reconsider that active
  routine's policy before resuming its native code.
- Preserve exact fuel accounting and asynchronous compilation. The poll
  interval remains a notification interval; document how transition limits
  separately cause the driver to reconsider policy.

Acceptance: request an applicable upgrade at the first relevant native
safepoint after crossing its threshold while useful work remains. The bounded
overshoot must follow the workload's poll spacing, not a million-instruction
CLI budget. The request must not depend on unrelated compiler completions.
Cover pending upgrades, failed upgrades, speed size rejection, and no downgrade
of an installed higher tier. Installation remains dependent on compiler
completion; the tests must distinguish requesting from installing.

Keep thresholds unchanged in this change so correctness and tuning can be
evaluated independently.

**3. Separate engine settings from compilation and dispatch structure.**

Primary files: `src/backend/cranelift_backend.rs`, `src/backend.rs`,
`src/main.rs`, and `benches/engines.rs`.

- Centralize the resolved internal configuration: optimization level,
  allocator, compilation scope, and resume/continuation strategy. Keep the
  existing CLI engine names and public backend entry points compatible where
  possible.
- Initially retain the known defaults: fast uses `none` plus backtracking;
  speed uses `speed` plus backtracking. Correct comments/help text that imply
  the current engines differ only in optimization flags.
- Build matched comparisons for both combined and per-routine compilation.
  Hold dispatch, resume entries, continuation strategy, and allocator constant
  when measuring optimization-level effects. Label production configurations
  separately when their resume strategies deliberately differ.
- Prefer the existing combined dispatch as the first candidate for a common
  eager-engine path, because it avoids the measured cross-artifact call cost.
  Validate combined speed on large programs and arbitrary-PC resumes before
  selecting it. Automatic JIT continues to need independently replaceable
  per-routine units.
- If combined speed has unacceptable compile-time/code-size growth, evaluate
  a shared per-routine eager path after step 4. Choose the layout from these
  measurements and record the decision; do not silently slow fast mode simply
  to make the two modes structurally identical.
- Measure the cost and necessity of continuation compilation separately.
  Retain arbitrary-PC resume support; defer or specialize continuation work
  only when an existing lower-tier/interpreter bridge preserves that contract.
- Include effective settings and structure in benchmark output, and in opt-in
  reports where useful, so results identify the actual configuration tested.

Acceptance: an optimization-only comparison uses equivalent execution
structures. The selected production eager path preserves output, failures,
instruction budgets, input suspension, and arbitrary-PC resumption. Changes to
layout must pass the performance gates below before becoming defaults.

**4. Reduce synchronization around native calls.**

Primary files: `src/backend/abi.rs`, `src/runtime/native.rs`,
`src/backend/cranelift_backend.rs`, and `src/backend/winch/emit.rs`.

- Define when a native callee returns with a fully materialized, current VM
  view and when it returns with pending writes/counters in a suspended frame.
- First implement a narrowly proved fast path that avoids repeating
  `finish_native`/`prepare_native` when a helper has already synchronized the
  return state. Retain synchronization for dirty state and deeper-frame yields.
- Apply the contract consistently to Winch and Cranelift calls, including
  mixed-tier calls. Avoid a broader native calling-convention rewrite unless
  remaining measurements justify it.
- Add diagnostics that distinguish attempted synchronization, actual work,
  and any skipped synchronization. Keep instrumentation opt-in.

Acceptance: the normal square/cube return path avoids redundant full
materialization, and unobserved warm call-heavy execution improves. Tests
must preserve budget yields inside callees, recursion past the native-stack
limit, BYREF aliases, errors, input suspension, GC stress, and code-version/root
lifetimes. Do not remove the existing flush without proving which return states
make it redundant.

**5. Recalibrate policy and publish verified behavior.**

Primary files: `src/backend/tiered.rs`, `src/report.rs`,
`docs/architecture.md`, `docs/benchmarks.md`, and CLI engine descriptions.

- Measure the corrected scheduler and selected engine structures before
  changing admission thresholds. Earlier requests can waste compiler work on
  short runs even though the requests are now semantically correct.
- Tune admission using measured compilation cost and execution savings across
  scalar loops, tiny leaves, recursion, arrays, strings, and cold routines.
  Record a break-even estimate for each supported workload class. Retain
  conservative speed eligibility until measurements support relaxing it.
- Keep the implementation bounded: region JIT, speculative inlining, a new
  native ABI, and adaptive online cost learning are follow-up work if the
  measured goals require them.
- Update reports to describe effective thresholds and settings. Document that
  `cranelift_speed` selects an optimization policy rather than promising the
  lowest end-to-end latency for every program.

Acceptance: requests meet the configured policy, defaults are supported by
measured cold/warm tradeoffs, and documentation matches the shipped structure.
Do not use “speed always beats fast” as a completion criterion.

**Verification and performance gates.**

Run targeted tests after each runtime change, then the workspace suite once
the implementation is integrated:

```sh
cargo test --release --test tiering --test engines --test native_emitters \
  --test native_storage --test metrics --test deferred --test gc
cargo test --workspace
cargo bench --bench engines
```

Use interpreter differential checks for output, instruction count, status,
errors/source locations, and resumed execution. Extend existing integration
fixtures rather than duplicating shallow tests of policy calculations.

For performance, rebuild the same revision with identical build settings, then
measure cold CLI runs and warm fixed-tier sessions without `--jit-report`.
Use at least 40 cold samples after warmup and rotate warm engine order. Record
median/p95 latency, isolated compilation, and code size. Use reporting in
separate runs to explain boundaries and promotion events.

Compare `modern.ya`, flat loops, recursion, arrays, strings, and the existing
sized algorithms. Treat a greater than 5% median or p95 regression that repeats
across three batches as a gate requiring investigation and an explicit tradeoff
decision. This is a proposed review threshold, not a wall-clock assertion for CI.
Use sufficient work per sample for very small fixtures.

Completion requires the missed-promotion regression to pass, equivalent engine
comparisons to be available, redundant return synchronization to be reduced
without semantic regressions, and fresh measurements/documentation for the
chosen defaults. Land the scheduler fix independently; subsequent performance
changes must remain separately reversible.
