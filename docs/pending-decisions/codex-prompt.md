Please finish writing the plan you were working on, and fold in the decisions and pending items below. Do not start implementing code; the plan is the deliverable.

State of the repo (main workspace /Volumes/Dev/x/cyallo, branch main; data-enum work is commit d6ebc04; overhaul is a separate line at 62aa665). Everything undecided or unfinished is written up in docs/pending-decisions/ (start with README.md). Read it before finishing the plan.

Decisions already made by the user:
- std types are described by declarations only; no compiler-owned StorageKind type system. Native types are opaque types bound through a per-runtime registry (provider bindings), not parser/AST hacks.
- A `core` module is always supplied (like Rust's core): Option, Result, numeric constants/arithmetic, struct utilities. It is a singleton and the one exception to "no single std". Host-bound things (channels, clocks, tasks, allocators) stay in SDKs/worlds.
- Option/Result/GeneratorStep/SendError/ReceiveError are real enums. Constructors are Option::Some/None and Result::Ok/Err only; the some/none/ok/err functions are removed. The new enum syntax is the only authority.
- Enum layout must come from a generic shape-based optimizer, not name or role special cases.
- Non-GC values are stack-like: their storage must be reused by scope and must not leak.
- The compile worker pool stays process-wide (recorded exception).

Pending, with questions for the user (details in docs/pending-decisions/):
- World/SDK work not built: embedder-intrinsic reference harness and safe emitter API, load handshake and generated W::Env/Exports/load, the prelude/init world, converting std into SDK packages (core, collections, tasks, ...), and updating docs/component-worlds to the core exception.
- core contents and scope; deleting the remaining StorageKind markers and old intrinsics; layout optimizer shapes; Cyallo-bodied methods on enums/structs (currently E173); scope-based cell release; the 15 baseline-failing tests; a 16-18% slowdown on the Option/Result loop (unprofiled); merging main and overhaul.

Mark anything in the plan that depends on those open questions as such instead of guessing.

This session now runs in auto mode (approvals reviewed automatically, workspace-write sandbox), not plan mode: write the finished plan file(s) directly, then reply with a short summary of what you wrote and where.
