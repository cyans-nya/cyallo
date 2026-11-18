# 08 — Performance and checks not run

- **Slowdown.** The Option/Result loop written in the new syntax takes about 2258 ms
  (interpreter) and 1588 ms (JIT), against 1907 / 1274 ms for the earlier helper-function
  program on the same tree and 1954 / 1339 ms on the baseline. The programs differ (enum
  constructor op instead of an intrinsic), so the 16-18% gap is indicative. Not profiled.
  Candidates: `EnumConstruct`/`EnumTest` paths, per-call frame setup for enum-typed
  registers, `Type` clones where `Arc` sharing is not yet used.
- **Not run:** Criterion benches (`engines`, `gc`, `objects`), Clippy, workspace all-features
  build, vendor checks. `cargo fmt --check` reports 241 diffs that predate this work.
- **Sizes to keep:** `Type` 48, `Value` 64, `Op` 80, `Instruction` 88 bytes (release).

**Decision needed.** Whether to run rustfmt over the repository (large diff) or leave the
existing style alone.
