# 07 — Failing tests carried since the baseline

The full root suite fails the same 15 tests as the pre-existing baseline (compared by name).
They fail in this tree and pass in the original workspace baseline because the standard enum
constructor functions the std package now emits shift function indices:

attributes_survive_parallel_parsing_and_specialization,
call_soon_coalesces_and_lazy_remains_eligible_for_observed_tiering,
declaration_errors_are_eager_and_analysis_is_shared_between_workers,
native_callees_are_charged_to_their_own_tier_and_caller_is_restored,
native_callees_request_fast_without_returning_to_the_driver,
never_and_always_lazy_are_honored_by_all_layouts,
optimized_budget_resumes_stay_in_speed_without_repeat_promotions,
osr_keeps_byref_aliases_to_caller_storage,
osr_promotes_a_running_invocation_without_restarting_it,
osr_report_tracks_same_invocation_and_executed_tiers,
recursion_and_checked_failures_remain_observable,
reports_use_real_frames_and_dynamic_targets_remain_dynamic,
returning_procedures_and_private_mutation,
speculative_work_is_bounded_and_lowered_rejections_wake_waiters,
stateless_procedure_values_do_not_execute_during_initialization.

**Decision needed.** Either emit enum constructor functions only when used (lazy emission,
better for startup and code size), or update the tests to not depend on function indices.
The crate test `payload_contracts_are_nominal_and_constructor_bodies_keep_variant_ids` now passes.

## Release-profile workspace regression

The verified v4 and scalar SDK fixture run reproduced all 15 root failures
above. Five additional `cyallo-ir` native-contract tests failed in both this work
and an isolated checkout of `8e1cbef`:

- `native_contract::tests::allocator_and_user_trait_evidence_is_retained_as_an_obligation`
- `native_contract::tests::borrowed_allocator_regions_survive_clone_weak_conversion_and_upgrade`
- `native_contract::tests::malformed_contracts_and_missing_evidence_fail_with_typed_errors`
- `native_contract::tests::native_callable_infers_return_only_generics_and_type_families`
- `native_contract::tests::ownership_and_generic_unit_overloads_come_from_source_headers`

Two fixture fixes qualify the registry alias lookup by its declaring module
and restrict the enum constructor assertions to the test package. After
those fixes, the workspace failure set matched the 20 inherited names.
The verified-bytecode, CLI, registry, enum-resolution, and scalar SDK
fixtures passed.

The v5 run with synchronous SDK host callbacks passed 703 tests and
reproduced the same 20 failures by name. The host fixtures and the verifier
case for an uninstalled host target passed.

The scalar call-result slice passed 706 release tests. It reproduced all
20 failures by name and added coverage for discarded direct and bound
results, including fuel, guest overflow, and host result validation.

At the 5A boundary, the final release workspace run passed 721 tests and
reproduced the same 20 failures by name. Scalar loan, cleanup, native
preparation, unlinked emitter, and image validation tests passed on macOS
AArch64. One earlier run also failed
`clean_native_call_returns_skip_materialization_but_dirty_yields_do_not`
at `boundaries.flush > 0`. It passed 30 isolated reruns, 25 runs of the
complete native-storage test binary, and the final workspace run. The
cause of that one failure remains unverified.

The final 5A and 5B release workspace run passed 728 tests and reproduced
the same 20 failures by name. The 11 scalar native fixtures passed on each
of macOS AArch64, macOS x86-64, Linux AArch64 and Linux x86-64. The x86-64
fixtures ran under translation on Apple Silicon. The Linux binaries ran
in Ubuntu 24.04 containers. The earlier parallel workspace run reproduced
the same intermittent legacy native-storage failure, which passed in the
final run. The local Winch provenance check and focused Clippy with warnings
denied also passed. [Native matrix](../../.github/workflows/native.yml)
