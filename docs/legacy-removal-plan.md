# Legacy removal plan (cutover phase 5)

State: every example runs on the verified engine; CLI `run`, `repl`, `debug` default to it
(`cyallo::vm::Session`). Legacy code remains for explicit `--engine`, JIT reports, the
simplified dialect, and 33 legacy-API test files.

## Inventory

Delete (≈ 21k lines):

| Area | Lines | Notes |
| --- | --- | --- |
| `src/runtime.rs`, `src/runtime/*` | ≈ 10.0k | legacy VM, services, generators, concurrency |
| `src/backend*` (not `backend/verified`) | ≈ 7.9k | legacy Cranelift/Winch/tiering |
| `src/report*`, `src/tracer*` | ≈ 3.6k | JIT reports, trace viewer |
| `src/bytecode.rs`, `repl.rs`, `inspection.rs`, `metrics.rs`, `admission.rs` | ≈ 1.1k | legacy-only |
| crates `cyallo-values`, `-shapes`, `-concurrent-values`, `-scheduler`, `-channel`, `-clock` | | only after `rg` shows no verified user |
| deps `winch-codegen` (+ `vendor/`), `wasmtime` | | drop with backend |

Keep: `src/dynamic_modules`, `src/vm*`, `src/bytecode/verified*`, `src/backend/verified`,
`crates/cyallo-{ast,parser,ir,modules,io,host}`, and the part of `src/constants.rs` the
verified lowering uses (check before deleting).

## Decisions (user, 2026-11-20)

1. Simplified dialect: give it a verified form (sparse arrays).
2. Chars and 32-bit numbers: add verified support.
3. JIT reports / trace viewer: rebuild over verified; low priority, not a removal gate.
4. Native tiers must eventually cover all code, not just scalars. Removal does not wait
   for it, but it is the main follow-up (phase 4 of the cutover).
5. Services/lookup/object caches: delay until native tier integration, unless they break
   existing code.
6. Tests: most legacy-API tests are removed, not migrated; recreate only the useful ones
   against `vm::Session` as fresh `verified_*`/`vm_*` tests.

## Blockers (original list, resolved by the decisions above)

1. **Simplified dialect** (sparse arrays) has no verified form: add it, or drop the dialect.
2. **Chars and 32-bit numbers**: add verified kinds, or drop from the language.
3. **JIT reports / trace viewer / `--no-inline`/notices**: re-implement over verified
   (image + disasm already exist) or retire.
4. **Native tiers**: verified native rejects all non-scalar kinds; after removal only the
   interpreter runs those. Acceptable if throughput is recorded.
5. **Unimplemented perf features**: services caches/stats, lookup caches, object ICs.

## Steps (each one commit, full test run, same-failures gate)

1. **Test triage** (no source deletion). Classify the 33 legacy-API files:
   - *language behavior* (gc, lookups, services, composites, generators, namespace,
     objects, modules, cells, data_enums, effects…): repoint to `vm::Session`, keep
     expectations.
   - *engine internals* (inlining, tiering, native_storage, metrics, engines,
     parallel_compilation, native_callbacks, object_fuzz stats): delete or rewrite against
     `backend/verified` tests.
2. **Decide blockers 1–3**, implement the kept ones.
3. **Make `Engine` verified-only** in `src/main.rs`: remove `--engine` legacy names,
   (the automatic fallback in `run` and the simplified-REPL path were already removed; explicit `--engine jit|interpreter|cranelift|cranelift_speed` remains until the legacy engine is deleted).
4. **Delete legacy runtime + backends + report/tracer**, remove their re-exports in
   `lib.rs`; fix compile.
5. **Prune crates and deps**; rerun `cargo tree` to confirm nothing dangling; delete
   `vendor/winch-codegen`.
6. **Promote names**: `vm::Session` → `Session`, drop `Verified*` prefixes where
   unambiguous; update docs (`overhaul-stages.md`, stage 10 numbers: storage, startup,
   throughput before/after).

## Gates

- `cargo test --release --offline --locked --no-fail-fast`: failures equal baseline.
- `cargo clippy --release --offline --locked --all-targets` clean.
- All examples run via `cyallo run` (also `--gc-stress`); output matches documented.
- Binary size and build time recorded before/after.

## Risks

- Hidden coupling (`constants.rs`, `dynamic_modules`, host crates) surfaces only at
  compile time: do step 4 as a deletion-only commit.
- Perf session owns `dynamic_modules.rs`, `ir.rs`, `analysis*`, `osr.rs`, `bytecode.rs`:
  message them before deleting `src/bytecode.rs` or touching those.

## Test migration status (2026-11-20)

Legacy-API tests were removed or ported to `cyallo::vm::{Context, Session, Status}`.
`native_callbacks` is language-level (callback inference, E177), not engine internals, and
was kept. `simplified` was deleted; recreate it when the simplified dialect has a verified
lowering (cases: sparse array growth, missing reads fail, inferred types fixed, strict mode,
sparse copies independent, nested sparse arrays, globals in calls, case ranges).
`tests/effects.rs` has two failures that predate this work (effect analysis, not the engine).

Ported tests that the verified engine cannot yet run are `#[ignore]`d with a reason; each is
a to-do for the engine, not a reason to drop the test:

- `cells::replace_transfers_noncopy_contents_and_assignment_drops_old_owners` — verified engine: assignment through a Cell loan does not drop the old owner
- `composites::records_tuples_and_explicit_clones` — verified engine gap; see docs/legacy-removal-plan.md
- `constants::frozen_aggregates_make_independent_mutable_copies` — verified engine gap; see docs/legacy-removal-plan.md
- `constants::fixed_shape_array_constants_are_copied_per_use` — verified engine gap; see docs/legacy-removal-plan.md
- `data_enums::borrowed_payloads_mutate_original_storage_and_guards_commit_once` — verified engine gap; see docs/legacy-removal-plan.md
- `lookups::paths_cannot_cross_receivers_and_erased_casts_are_checked` — verified engine gap; see docs/legacy-removal-plan.md
- `lookups::mapper_effects_and_escaping_borrows_are_rejected` — verified engine gap; see docs/legacy-removal-plan.md
- `managed::fallible_allocation_returns_errors_and_drops_the_payload` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::utilities` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::invalid_utility_domains_match` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::for_in_ranges_iterate_without_overflow_in_both_directions` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::casts_bind_looser_than_unary_operators` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::strings_concatenate_with_plus` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::numbers_follow_rust_like_rules` — verified engine gap; see docs/legacy-removal-plan.md
- `modern::tuples_records_and_arrays_compare_structurally` — verified engine gap; see docs/legacy-removal-plan.md
- `native_callbacks::return_only_native_generics_are_determined_by_the_callback_signature` — verified lowering: function-typed local with Result/enum payload (R900)
- `objects::nested_views_are_checked_before_establishing_any_contract` — verified engine gap; see docs/legacy-removal-plan.md
- `postfix_dereference::dereference_chains_preserve_index_tuple_call_cast_and_multiplication_precedence` — verified lowering: array of shared references (R900)
