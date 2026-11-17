# Intrinsic catalogue audit (phase 2c)

Recorded 2026-11-17 from a read-only audit (contracts: [intrinsic-contracts.md](intrinsic-contracts.md)) of the `data-enums` worktree. Counts of
variants are exact; per-family groupings and the Builtin/Lookup split are approximate
(±1-2). Input to the implementation-table and engine-intrinsic design in the
[SDK/worlds plan](component-worlds/README.md).

## Findings

- **No intrinsic is inlined by a native backend.** `Intrinsic` has 100 variants
  (`crates/cyallo-ir/src/intrinsic.rs`) and `MemberIntrinsic` 75
  (`crates/cyallo-ir/src/internal.rs`); `abi_keys!` binds 219 private keys. The
  Cranelift and Winch emitters inline only generic ops: scalar load/store/move,
  scalar unary/binary, literals, addresses, `CheckStep`, jumps/branches,
  `ServiceKey` and `Service::Reference`. Every `Op::Builtin`, `Op::Lookup`,
  `Op::Dynamic` and other `Op::Service` goes through `native_step`
  (`src/backend/abi.rs`) into the interpreter.
- **Inlining is blocked by coarse metadata.** `gc_metadata.rs` marks Builtin,
  Lookup, Dynamic, most Service, Call and enum ops `may_collect`/`may_reenter`,
  and backends only inline instructions that are neither. `EnumConstruct`,
  `EnumTest` and `EnumPayload` are not handled by either backend, so Option/Result
  tests and payloads also run in the interpreter.
- **There is no per-intrinsic contract.** Behaviour is spread over
  `Intrinsic::consumes_argument`, `builtins::pure` (one boolean), hardcoded
  `builtins::result_type`, `lookup::Operation::writes`, `ownership.rs:620-690`,
  per-Op `EffectSummary`, and per-Op `SafepointEffects`. Nothing records may-park,
  per-intrinsic may-fail types, or GC-root behaviour; pending/park handling is ad hoc
  in the runtime (Input, channel receive, sleep, join, generators).
- **Only scalar object property access has a specialization.** Object ICs
  (`docs/native-object-ics.md`) cover `Op::Dynamic Get/GetKey/Set/SetKey` on scalar
  fields with four cached shapes and a checked fallback. Function inlining
  (`src/backend/inlining.rs`) is limited to integer/boolean routines. There is no IC or
  guarded fast path for collections, Option or Result.

## Classification (about 100 + 75 operations)

| Class | Operations | Direction |
| --- | --- | --- |
| Pure, expressible as Cyallo source (~40 intrinsics, ~15 members) | Numeric Abs/Min/Max/Clamp/Round/Floor/Ceil/Pow; char conversions; string Left/Right/Mid/Trim/Upper/Lower/Contains/StartsWith/EndsWith/Replace; Sum/Count/Keys/Values/StringFrom/Parse; Option/Result `is_*`/`unwrap`/`or_fail`; collection length; range reverse; generator/task `is_*` readers | SDK source bodies over a few primitives (string index/slice, float ops, enum tag/payload) |
| Scalar/cast primitives (~12 casts, Sqrt, Float*) | CastI8..CastF64, sqrt and float variants | Engine intrinsics, one inline op each |
| Engine-owned memory/GC | Allocate/Deallocate, Layout, Rc*/Gc*/Weak, Clone/Drop, Cell, Location/StoragePath, collection push/pop/slice/literals, Option/Result constructors, Erase/Checked/FreshSymbol, ObjectView/Freeze, service registration (9) | Engine intrinsics with declared effects, roots and fast paths; the surface a layout/IC optimizer must see |
| Host / side-effecting | Print, Input, Random, ModuleLoad, clock, scheduler (TaskSpawn, join, cancel), channels (9 members), generator start/resume/close (interpreter-frame coupled) | Host calls with declared effect class and Pending protocol |

## Consequences for the design

1. **Today every native op is an opaque call.** Moving library behaviour into SDK
   source cannot regress native code for the ~55 pure/readable members and builtins,
   because they are already opaque; it can improve it once the optimizer sees tag
   tests, scalar math and string primitives.
2. **The catalogue needs a contract table first.** Before an embedder (or std) can bind
   a bodyless item to an intrinsic safely, each intrinsic needs one record: operand
   modes, result type rule, effect class (pure / reads / writes / allocates / may-fail /
   may-park / nondeterministic), GC-root and safepoint behaviour, fallback body, and
   per-engine lowering availability. This replaces `consumes_argument`, `pure`,
   `writes`, the ownership match and the coarse safepoint rule.
3. **Safepoint metadata must become per-intrinsic.** Inlinable engine primitives
   (tag access, scalar math, buffer load/store) must be marked non-collecting so the
   existing `!may_collect && !may_reenter` gate admits them.
4. **First lowering targets** (largest win, smallest surface): enum tag/payload ops
   (Option/Result/any enum), the 12 casts, scalar numeric builtins, collection length
   and indexed load/store, and bounds-checked push with a cold grow path.
5. **The binding keys should shrink.** After migration only genuinely host/engine
   items need a binding (about 60-70 of the 219 keys); the rest become source bodies.
6. **Not determined:** the exact Builtin-versus-Lookup assignment per variant,
   whether `Length`/`ArrayLength`/`StringLength` always lower to `Op::Length`, and
   whether the `wasm-adapter` feature and `src/backend/tiered` change any of this.
