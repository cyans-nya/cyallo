# Native baseline and compilation scheduling plan

Status: implemented on ARM64 macOS, with native Linux ARM64/x64 runtime CI gates configured but not executed locally. The bytecode-native baseline replaces the Wasm adapter by default; the adapter remains feature-gated for differential benchmarks. See [architecture](architecture.md), [measurements](benchmarks.md), and [vendor maintenance](../vendor/README.md) for the implemented contracts. The sections below retain the design and upgrade acceptance criteria; Windows x64 runtime validation remains outstanding.

## Target

```text
typed register bytecode
  ├─ interpreter
  ├─ native baseline: CAIE frontend + vendored Winch machinery
  ├─ Cranelift fast
  └─ Cranelift speed
```

The native baseline consumes our bytecode directly and uses the same scalar storage, entry ABI, source spans, runtime slow paths, and VM-frame OSR contract as Cranelift. Production baseline execution will not generate/parse Wasm, instantiate Wasmtime modules, or call Wasmtime host imports for scalar instructions. The old Wasm adapter remains temporarily available for comparison.

The scheduler uses synchronous Kanal channels with explicit priorities and request coalescing. Channel transport is a small part of this work; removing per-bytecode runtime boundaries is the main execution improvement.

## Before-change evidence and constraints

- The old Wasm baseline calls `state` on every bytecode dispatch. A lowered addition also calls `get` twice and `put` once. Most other operations call the interpreter's `step` helper.
- The old compiler service used a single FIFO worker. `info.compiling` causes higher requests to be ignored until the driver retries. A recent trace requested speed after 87,161 instructions despite the then-current 10,000-instruction hotness threshold.
- The pinned package is `winch-codegen` 50.0.1, upstream commit `c307ec81de02852d815e9849f7c89d955c03c30d`, path `winch/codegen`. Its public compile entry accepts Wasm signatures/bodies, validators, and module translation. Its private register allocator and assemblers are reusable ingredients; its Wasm operand-stack frontend and VM-context-dependent prologue are not a generic native frontend.
- Our pinned `cranelift-module`/`cranelift-jit` 0.137.1 support `define_function_bytes`. The native frontend now publishes Winch-emitted bytes through that memory owner with resolved local fixups and absolute helper pointers; it does not introduce an executable-memory implementation.
- Compilation remains separate from execution. No compiler job owns or mutates a live Session. Installation and code retirement remain execution-thread operations at safepoints.

## 1. Capture a baseline and expose the remaining costs

Measure forced interpreter, old Winch, Cranelift fast, and Cranelift speed using fixed tiers. Separate cold CLI wall time, compiler startup, queue delay, code emission, publication/instantiation, first entry, and warm execution. Use small/long loops, calls/recursion, BYREF aliases, arrays, and strings. Measure medians and tail latency without observation enabled; trace/report runs explain behavior separately.

Add optional runtime-boundary counters so a scalar loop's reduction from per-bytecode helpers to genuine safepoints is measurable. Preserve the current OSR and per-tier instruction metrics.

## 2. Introduce a Kanal scheduler with durable desired tiers

Replace the boolean compiling gate with per-function state: desired tier, queued/running tier, installed tier, request generation, and current hotness. Identify work by validated-program identity plus function ID. A request updates the highest desired tier immediately, even while a lower tier is compiling.

Keep the authoritative pending map and priority heap in a compiler service. Kanal transports coalesced wakeups and completion notifications; it does not provide the priority ordering. A bounded wakeup channel with `try_send` lets execution continue when a wakeup is already pending. Pending work lives in the map, so a full notification channel cannot lose an upgrade. Completed artifacts remain owned by a per-artifact mailbox until polled; notifications do not block the worker behind an idle execution thread.

Prefer hot active-function upgrades over cold baseline requests, with aging so ordinary requests are not starved. Re-read desired tier immediately before starting a job: a queued Winch request can become a speed request without compiling the obsolete baseline first. An already-running job finishes normally. On completion, schedule any higher desired tier immediately, without waiting for the next VM poll to request it.

Allow useful lower-tier code to run while an upgrade is pending, but never install a lower tier over a higher installed tier. Reject results for obsolete program generations. Preserve cancellation when the owning artifact is dropped. Use `min(3, logical CPUs - 1)` workers by default, with a floor of one on single-core/unknown hosts. Priority cannot preempt an in-flight compilation. No separate optimizing pool is used.

Add timeline events for desired-tier changes, reprioritization, superseded jobs, generation rejection, and installation. At a call-entry safepoint, return control to the tier driver early enough to discover a new active callee instead of interpreting an entire quantum first. Avoid eagerly compiling one-shot cold wrappers unless the admission policy predicts a benefit.

## 3. Vendor Winch and prove a standalone native entry

Vendor the pinned codegen crate under `vendor/`, preserving its license and recording the upstream commit and local patch list. Add a narrow native compilation entry inside the fork; keep CAIE-specific bytecode decoding in a first-party frontend. Reuse assembler, register-set, and allocation machinery where it fits. Adapt spill handling for register bytecode instead of pretending it is Winch's Wasm operand stack.

First prove AArch64 emission on the current host: a checked integer loop, a native C-ABI return, and one safe runtime call. Implement a platform-correct prologue/epilogue, stack alignment, saved registers, and stack limits without a dummy Wasm module or VM context. Verify raw-byte publication, relocations, instruction-cache synchronization, and artifact lifetime through the existing JIT memory owner. Then implement/test x64. Unsupported targets use an explicit reported Cranelift-fast fallback until supported.

This phase is a feasibility gate. If reuse requires invasive changes across unrelated Wasmtime subsystems, document that result before expanding the fork; the plan does not assume Winch is already a standalone assembler API.

### Upstream tracking and fork maintenance

Treat the vendored tree as a reproducible import plus a small patch series, not an independently evolving copy. Establish the following layout and tooling with the first vendoring change:

```text
vendor/UPSTREAM.toml
vendor/winch-codegen/                       # generated imported tree + patches
vendor/patches/winch-codegen/series          # ordered patch list
vendor/patches/winch-codegen/0001-*.patch
src/backend/winch/                         # first-party bytecode frontend
src/runtime/native.rs                      # shared runtime adapter
xtask/                                     # import/check/update tooling
```

`UPSTREAM.toml` records repository URL, upstream release/package version, exact commit and source subdirectory, source checksum, patch-series checksum, license/notice files, enabled features, and the tested dependency/toolchain versions. Start from the existing 50.0.1 commit above. Pin the compatible Winch, Cranelift emitter/JIT, and Wasmtime-internal dependency set together; their internal APIs are coupled. Where a manifest must be adapted from upstream workspace dependencies, encode that adaptation as a reproducible patch rather than an undocumented edit.

Keep bytecode decoding, CAIE semantics, register-bytecode spill policy, OSR mappings, scheduling, and `NativeView` adaptation in first-party crates. Restrict upstream patches to the narrow native emission seam, required visibility/extension hooks, and platform ABI support that cannot live outside Winch. Prefer additive APIs and native-specific modules so existing Wasm behavior stays intact. Avoid reformatting or renaming upstream files. Each patch records its purpose, affected upstream subsystem, originating upstream commit when applicable, and its removal condition. Generic hooks can be prepared for upstream contribution; the maintenance plan does not assume they will be accepted.

Provide `cargo xtask vendor-winch` commands for reproducible import, verification, and candidate updates. Verification reconstructs the tree from the recorded source plus ordered patches in a temporary directory and compares it with `vendor/winch-codegen`; CI fails on unrecorded edits, checksum mismatches, or patch application conflicts. Patch regeneration and base-pin changes must occur in the same change, so the checked-in source always has a reproducible provenance. These commands now exist; see `cargo xtask vendor-winch --help`.

Use a weekly automated check for new upstream releases and relevant changes to the ISA emitters, allocator, ABI/frame handling, relocation/publication interfaces, and dependency requirements. Production follows an exact reviewed stable-release pin. An optional upstream-main canary can build against a coherent candidate dependency set to reveal upcoming breakage; it never changes production pins or publishes runtime artifacts. Applicable correctness or security fixes can be backported promptly as individually recorded patches without waiting for a full release upgrade. Upstream checks create a candidate update report/branch; no automatic merge or production-pin advance is allowed.

For each candidate upgrade:

1. Fetch the new immutable upstream revision, verify provenance, and summarize relevant changes between the old and new bases. Include the compatible dependency/toolchain changes rather than upgrading Winch alone.
2. Reapply the local patch series without silent fuzzy matching. Resolve conflicts explicitly, remove patches superseded upstream, and update the patch rationale/removal conditions. Refresh the manifest adaptations and import/check metadata.
3. Review changes to emitted arithmetic, register allocation/spills, calling conventions, stack checks, branch fixups, relocations, source locations, and executable-memory publication. A clean patch application is not evidence that semantics stayed compatible.
4. Run upstream tests for the reused components and our native baseline/interpreter/Cranelift differential tests. Require runtime validation on native AArch64 and x64 hosts for targets we claim to support; cross-compilation alone does not validate emitted code. Include arbitrary-PC OSR, tiny budgets, BYREF aliases, mixed-tier calls, cold errors, and code-lifetime tests.
5. Compare compilation latency, cold CLI time, code size, warm execution, and queue/OSR behavior against the recorded previous pin with reporting disabled for performance measurements. Record median/tail results and investigate material regressions before advancing the production pin.
6. Land the new pin, dependency lockfile, refreshed patches, provenance metadata, test results, and benchmark comparison together as one reviewable upgrade. Retain the previous known-good combination in history so the entire upgrade can be reverted together.

If an upgrade cannot pass these gates, keep the previous pin and document the blocker; selectively backport needed fixes with regression tests. If the fork starts requiring broad rewrites across unrelated Winch/Wasmtime components, reassess the reuse boundary instead of accumulating an unbounded patch stack. Removing the old Wasm runtime adapter does not remove provenance, patch reconstruction checks, or upstream compatibility testing.

## 4. Lower scalar blocks directly

Use the common `(Session*, NativeView*, budget) -> remaining_fuel` ABI. Pre-index labels and basic-block boundaries, then emit in one forward pass without SSA construction or heavy optimization. Start with canonical scalar slots, then add block-local register caching and cheap use counts to reduce loads/stores. Spill cached values at joins and safepoints; do not cache addressable cells across possible BYREF aliases without proving it safe.

Emit constants, scalar addresses/loads/stores, initialization checks, moves, comparisons, branches, checked integer arithmetic, and REAL arithmetic directly. Arithmetic must use CAIE semantics: signed overflow errors, checked negation, zero-divisor handling, and `MIN MOD -1 == 0`. Winch's wrapping Wasm arithmetic is not a substitute.

The initial cache reuses the last scalar result within straight-line regions and writes through to canonical slots. Arbitrary-PC landing stubs restore its expected value. This deliberately avoids coupling our register-bytecode policy to Winch’s Wasm stack allocator.

Use native branch labels in normal flow. The resumable-PC dispatcher runs at entry/resumption rather than once per bytecode. Runtime helpers remain for allocation, call setup/return initially, complex values, I/O, and cold errors. Scalar loops must have no `get`/`put`/`state`/`step` calls in their loop body.

## 5. Preserve exact budgets, OSR, and calls

Keep bytecode instruction counts and diagnostic PCs exact, including failures and budgets smaller than a block. Start with cheap native fuel checks; add block-level charging only with a precise short-budget/error path. Never execute beyond the caller's budget to reach a convenient safepoint.

Every supported resume PC needs an entry mapping that reconstructs the block's machine-register state from canonical scalar slots. Spill before yielding, calling helpers, or switching tiers. Register caching must not reduce OSR to function entries or restart a live invocation. Preserve BYREF cell IDs, return destinations, and prior side effects.

Install native baseline entries in the existing stable entry table so baseline and Cranelift routines can call each other. Preserve bounded machine-call nesting and yield through VM dispatch when necessary. Reload frame/storage pointers after calls that may allocate or change the active frame. Keep exact per-tier metrics when native callees use a different tier.

## 6. Rollout gates and validation

- Deterministic scheduler tests: upgrade while queued/running, full notification channel, priority aging, owner cancellation, stale generations, and no downgrade.
- Runtime parity: scalar boundaries, errors/spans, input suspension, sparse holes, BYVAL snapshots, recursive calls, aliases through storage growth, and mixed-tier calls.
- OSR tests: enter/resume at every legal bytecode PC; budgets 0, 1, partial-block, and large; preserve one invocation and one-time entry effects.
- Inspect emitted code and helper counters: no Wasmtime host transitions or per-instruction state dispatch in scalar regions.
- Publish fixed-tier and cold-process measurements on AArch64 and x64. Native baseline must beat interpretation on the scalar workloads and provide a compilation-latency benefit over Cranelift fast before becoming an automatic policy choice. These are acceptance gates, not promised speedup factors.
- Keep explicit `cranelift` and `cranelift_speed` engines. Switch default Winch tier selection to the native frontend only after parity and performance gates pass. Remove the old Wasm runtime path afterward; Wasm text may remain an inspection/export utility if useful.
- Require reproducible vendor reconstruction and the upstream-update validation workflow before enabling the native baseline by default. Dependency updates must preserve the tested compiler cohort and cannot silently replace the vendored revision.

Suggested delivery order: measurement and scheduler as one reviewable change; vendored native-entry proof together with provenance, patch reconstruction tooling, and update checks; scalar lowering; OSR/mixed calls and target coverage; measured policy tuning and rollout. No speculative second compiler pool or threshold tuning before the first measurements.

## Sources

- [Kanal upstream API/source](https://github.com/fereidani/kanal), inspected for synchronous and non-waiting notification operations. No comparative channel-speed claim is assumed.
- [Pinned Winch ISA entry and calling conventions](https://github.com/bytecodealliance/wasmtime/blob/c307ec81de02852d815e9849f7c89d955c03c30d/winch/codegen/src/isa/mod.rs), checked against the local 50.0.1 package.
- Local pinned Winch `regalloc.rs`, `masm.rs`, AArch64 codegen, and Cranelift module/JIT raw-byte definition implementations were inspected for the proposed seam.

## Added first-call admission and deferred bodies

Small syntax bodies containing loops/calls receive bounded first-call compilation admission (default 1 ms). Compilation stays on the worker pool. `run` with automatic JIT or interpretation parses every body but defers body inference, lowering, purity checks, liveness, and bytecode generation until first use. Checked results are immutable and shared through OnceLock. Declarations and call signatures remain eager; `check`/inspection/explicit eager engines/REPL validate all bodies. Cold-call resource effects remain conservative and marked transitively incomplete; no optimization assumes an unchecked body is effect-free.

The CLI and benchmark executable use mimalloc. Embedders retain allocator ownership.
