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
   the fallback in `run`, the simplified-REPL path.
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
