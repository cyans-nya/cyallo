# 01 — The always-supplied `core` module

**Decided (user, 2026-11-17).** `core` is always supplied, like Rust's `core` crate: one
singleton, always available, holding `Option`, `Result`, the numeric constants and
arithmetic, and a few struct utilities. It is the one exception to the "no single std"
framing in `docs/component-worlds/`; everything that needs the host stays in SDKs/worlds.

**Current state.** `Option` and `Result` still live in `std/option.ya` and `std/result.ya`;
the prelude re-exports them as `prelude::core::*`. The compiler names their variants by
reserved declaration ids (`VariantId::OPTION_SOME` ... `RECEIVE_EMPTY`, ids 0-10, ordinary
ids start at 16). There is no `core` package.

**To do.**
1. Create the `core` package (embedded, always linked, not shadowable, versioned with the
   compiler) and move `Option`, `Result`, `GeneratorStep`, the error enums, numeric
   constants and struct utilities into it.
2. Make compiler-known paths come from `core` (`core::option::Option`, ...), replacing the
   `std::option::Option` strings hard-coded in the compiler.
3. Keep host-bound declarations (channels, clocks, tasks, allocators) outside `core`.
4. Record the exception in `docs/component-worlds/` and `docs/compiler-and-runtime-rules.md`.

**Decision needed.**
- Exact contents beyond Option/Result: which numeric constants and struct utilities?
- Does user code import `core` explicitly, or is it in scope everywhere (including bare
  `Some`/`None`/`Ok`/`Err`, which today need `use prelude::core::*`)?
- Startup cost: always linking `core` must stay inside the startup budget (CLI startup was
  about 8 ms); it should be pre-resolved and cached.
