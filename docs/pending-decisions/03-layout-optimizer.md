# 03 — Shape-based enum layout optimizer

**Decided (user).** A generic, shape-based layout optimizer replaces special cases. No
name- or role-based magic.

**Current state.** The runtime special-cases the core enums by role: `Runtime::store` keeps
them inline in their cell when `enums::is_core` is true (first variant has an `Option*`,
`Result*`, `Step*`, `Send*` role tag), while other enums store each payload in a child cell.
That is the role-based special case the decision rules out.

**To do.**
1. Define the layout decision from the enum's shape: payload-free variants, single payload,
   copy payloads, size/alignment, niche availability.
2. Use it for both core and user enums so `is_core` disappears.
3. Re-measure with the Option/Result loop (`docs/benchmarks.md`).

**Decision needed.** Which shapes get inline storage (all enums whose payload is a single
copy value? up to what size?) and whether niche filling (for example `Option<rc T>`) is in scope.
