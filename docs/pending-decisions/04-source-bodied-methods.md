# 04 — Cyallo bodies for methods on enums and structs

**Decided.** Methods that need no host support (`is_some`, `unwrap`, `map`, ...) are
implemented in Cyallo in `core`; only host-dependent items are bodyless and bound by the
per-runtime registry.

**Current state.** Source-bodied receiver methods on types are rejected (`E173`), so the
helpers are native members in `std/option.ya` and `std/result.ya` bound to member
intrinsics.

**To do.** Allow inherent `impl` bodies on enums and structs; then move the helpers into
Cyallo and delete the corresponding member intrinsics (`OptionIsSome`, `ResultUnwrap`, ...).

**Decision needed.** Failure behaviour of `unwrap`/`or_fail` written in Cyallo (the native
versions raise typed runtime errors with source spans); how that is expressed in source.
