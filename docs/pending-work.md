# Pending implementation work

Handoff recorded 2026-11-17. These are the agent's outstanding tasks, not work
assigned to the user. Execution history is in [the work log](data-enums-work-log.md).

## Current state

- Branch: `data-enums`; worktree: `/Volumes/Dev/x/cyallo/.worktrees/data-enums`.
- The worktree is uncommitted and includes copied module/compiler foundation
  changes. Preserve those and the independent changes in the original workspace.
- Current source declares `TaskHandle<T>` and `JoinResult<T>`. The AST/IR/runtime
  still contain TaskOutcome names and StorageKind cases. The rename is incomplete.
- The worktree currently does not build. A fresh
  `cargo check --workspace --all-features --quiet` failed in runtime/enums.rs on
  eight new task/failure encodings and in runtime/lookup.rs on NativeEnum,
  HostError and PanicPayload values. Log: `/private/tmp/enums-handoff-check.log`.
- Do not complete the rejected representation just to silence those errors.
  Replace or remove the unfinished task-specific additions while retaining the
  working general enum machinery.

## Agreed constraints

- A handle is TaskHandle; the return value of join is JoinResult.
- Standard names, nominal identity, generic parameters, fields, enum variants,
  methods and trait contracts belong to ordinary source declarations.
- Replacing StorageKind with another catalogue of the same library types does
  not meet the requirement. Private runtime operations and optimized layouts may
  exist, but they must consume resolved declarations and checked contracts.
- Generator abstractions must use the general trait machinery rather than a
  compiler-owned substitute for the public type. Audit the missing trait/generic
  capabilities needed to express those contracts.
- Preserve the intended stackful coroutine model. Poll was an illustrative
  comparison, not a request for Poll, Future, async/await, or a polling interface.
- Runtime state remains instance-owned; failures remain structured typed values.
- Continue logging implementation steps, failed checks and verified results.

## Ordered implementation tasks

1. **Replace the type/representation boundary.** Define and implement a general
   binding for opaque native resources and their operations. Keep source identity
   independent of representation, with ordinary aliasing, generic instantiation,
   visibility, traits and module publication. Remove the compiler's parallel
   standard-library type system; moving its list out of the AST is insufficient.
2. **Supply the required general language capabilities.** Existing trait dispatch
   covers a limited subset; docs/services.md still lists generic traits,
   associated types and default methods as absent. Determine and implement what
   the existing generator/collection contracts require. Do not add a separate
   special case for each abstraction in place of a missing language capability.
3. **Migrate every existing StorageKind family.** The audit must cover Cell,
   Location, StoragePath, Option, Result, Vec, VecDeque, Range, ParseError, Weak,
   ArenaCell, Layout, Allocation, AllocError, Generator/PureGenerator, Step,
   Sender, Receiver, SendError, ReceiveError, Clock, TaskHandle, JoinResult,
   TaskFailure, HostError, PanicPayload, TypeDescriptor, ViewError and LoadError.
   Use ordinary records/enums/traits where applicable and the general native
   binding for actual host resources. Names and signatures must come from source.
4. **Finish JoinResult and task failures on that foundation.** Remove TaskOutcome
   from the AST/IR type model rather than introducing a JoinResult storage case.
   Convert scheduler completions at the runtime boundary into the actual declared
   variants. Preserve Completed(T), Failed(TaskFailure), Cancelled and Stopped;
   Completed(Result::Err(...)) remains ordinary completion. Preserve Guest,
   Panic, Destructor and combined Cleanup causes without flattening them into
   strings. Verify payload identity, recursive ownership and GC roots.
5. **Finish the standard enum/error migration.** Rework Option, Result, Step and
   channel errors to remove their remaining special type markers. Finish
   ViewError and LoadError with structured payloads and correct open-enum
   coverage. Review ParseError and AllocError under the same rules. Convenience
   methods should use the ordinary language machinery where expressible.
6. **Update all consumers of the replaced types.** Name/type resolution,
   specialization, native contract validation, ownership/effect/transfer checks,
   constants, typed IR, bytecode, interpreter/native execution, cloning/drop,
   moving GC, detached task/channel transport, deferred checking, module caches,
   publication/relocation and REPL persistence must retain the same source type
   identity and contract. No source-name dispatch or magic semantic integer tags.
7. **Verify extensibility, not just std behavior.** Exercise equivalent user
   records/enums/traits, generic dispatch and vtables; aliases, shadowing and
   renamed/moved trusted declarations; malformed native bindings; closures,
   borrowing and managed payloads. Optimization must retain checked fallback,
   effects, failure behavior, roots and instruction accounting.
8. **Finish examples and documentation.** Add the task-result/failure Cyallo
   example; update concurrency examples and expected output. Correct stale
   TaskOutcome/TaskResult references and completion claims. Keep the distinction
   between language semantics and private runtime implementation explicit.
9. **Run final verification.** Eager/deferred checking; interpreter, Winch,
   Cranelift and Cranelift speed; moving GC stress; continuous and single-step
   engine handoffs; task/channel suspension and cleanup. Then workspace/all-feature
   tests, warnings-denied Clippy, formatting, diff checks and Winch vendor checks.
   Prior successful runs do not verify the current broken patch or its replacement.

## Previous verification to retain, not overstate

Before the unfinished task patch, the status regression batch passed 12
concurrency, 13 data-enum, 10 generator and 14 native-declaration tests. All 59 IR
unit tests passed after repairing the fixture pipeline. An earlier full workspace
run exposed and led to fixes for a compiler-worker stack regression and those
fixture failures; a successful full workspace run on the final state is still due.
Earlier Clippy and vendor checks also need rerunning after the replacement.

## Subsequent draft work

Component worlds and types-as-values remain separate, unimplemented draft work
from [the draft index](toc-new-drafts.md). This includes component/module values,
provided environments and authority, worlds/SDKs, versioning/loading/lifecycle,
SDK coordination and component recipes; then compile-time type values/type
functions, reflection and runtime type building. Do not claim these were completed
by enum support. Data-enum draft questions such as `?`, `let ... else`, anonymous
enums and explicit discriminants remain undecided rather than approved tasks.

Earlier modules, host-controlled static/dynamic imports, service lookup and
parallel compilation work must remain compatible with the new type boundary.
Their documents describe existing work; this handoff is not a new audit or a
claim that every earlier requirement has been verified.
