# Data enums: work log and resume checklist

> **Historical (2026-11-20).** This log was kept while the work lived on the `data-enums` branch and worktree, which are now merged into `main`; its resume checklist no longer applies. Current open items: [pending decisions](pending-decisions/README.md) and [overhaul stages](overhaul-stages.md).

Branch: `data-enums`.
Worktree: `/Volumes/Dev/x/cyallo/.worktrees/data-enums`.
Target: [data enums draft](data-enums-draft.md).
Implementation status: [implementation tracker](data-enums-implementation.md).

## Objective and completion criteria

Implement real, generic data enums. User enums and standard `Option<T>` /
`Result<T, E>` must use the same declaration, constructor, matching, ownership and
execution machinery. Source enum support alone is not completion. Do not encode
user enums as synthetic records with integer tags or dispatch by source names.

Completion requires executable Cyallo examples and rejection tests, eager/deferred
checking, interpreter and native engines, moving GC stress, instruction handoffs,
and passing applicable workspace checks.

## Ordered checklist

- [x] S01 Create isolated worktree; preserve the original workspace and carry
  forward the checked-module foundation.
- [x] S02 Add explicit enum/variant/pattern ASTs and anonymous record syntax.
- [x] S03 Parse generic/open enums, annotations, payloads, `match`, `if let`,
  patterns, guards and block tails; preserve source spans and parse-job ordering.
- [x] S04 Verify frontend and add explicit rejection of unfinished execution.
- [x] S05 Resolve enum and variant declaration identities; aliases/reexports;
  typed enum/constructor catalogues and nominal identity checks.
- [x] S06 Instantiate generic payload contracts, constructor type inference and
  contextual generic arguments; integrate checked-module dependency keys.
- [x] S07 Implement structural anonymous record checking, value layouts, source
  evaluation order, field access and nominal/structural distinctions.
- [x] S08 Implement typed enum construction, value representation, bytecode/VM
  operations, native helper/fallback paths, GC roots and constant values.
- [x] S09 Resolve pattern-local bindings; payload/tuple/record/literal patterns;
  alternatives and guards; arm result checking; exhaustive decision trees.
- [x] S10 Implement `if let`, open-enum package checks and useful diagnostics.
- [x] S11 Enforce moves, shared/mutable borrows, partial payload moves, guard
  commit order, branch cleanup and capture/escape constraints.
- [ ] S12 Finish standard `Option` and `Result` migration. Source enums and their
  behavior were verified; removal of the rejected special type model is pending.
- [ ] S13 Migrate remaining standard statuses/errors, retaining typed causes and
  resource payloads. Track these separately from the Result milestone.
- [x] S14 Add runnable `examples/enums/` programs and rejection coverage.
- [ ] S15 Verify all engines/checking modes, GC stress and one-instruction
  handoffs; run workspace tests, Clippy, format and vendor checks; update status
  from observed behavior only.

## Progress entries

### 2026-11-16 — Previous work established (S01–S04)

Created the worktree from `f51c909` and copied the uncommitted module/compiler
foundation it needs, excluding unrelated editor work. Added source syntax and AST
support. Added 12 parser tests covering declarations, patterns, annotations,
structural type order, literal evaluation order, visitors, source relocation,
parallel parsing and recursion limits. Added a transitional integration test
ensuring eager/deferred compilers reject unsupported forms instead of discarding
unused enum declarations. No enum VM support or standard enum migration exists.

Verified: 576 workspace tests passed; Clippy with warnings denied, formatting and
Winch vendor checks passed. Subsequent diagnostic/lint adjustments were checked
with the frontend and focused integration suites. These checks establish the
frontend foundation only.

### 2026-11-16 — Durable checklist created; S05 started

Recorded the full implementation sequence and completion criteria before further
code changes. Next work is declaration/variant identity resolution and constructor
contracts. Keep recording meaningful changes, failed checks, fixes and verification
results below. Leave a precise resume point at each checkpoint.

## Resume point

The consolidated, ordered handoff is [pending work](pending-work.md), recorded
at the user's request. It includes the current build failure and supersedes
obsolete implementation directions in the chronological entries below.

Active step: S13, with an architectural correction requested by the user.
The result of join is named JoinResult, not TaskOutcome or TaskResult.
Do not add a JoinResult StorageKind or merely move/rename the builtin type list.
Ordinary source declarations must own nominal identity, names, layouts and
signatures. Remove the task-result/failure storage markers first; the remaining
StorageKind-based std types also require replacement by source types and a
general native-resource/representation binding. Earlier claims that private
Type::Storage markers completed the migration were too broad.
The user's Poll reference was illustrative, not a request to introduce Poll,
Future, async/await, or a polling-based coroutine interface. Preserve the intended
stackful coroutine model. The correction concerns ordinary source declarations
and traits versus compiler-owned substitutes, not changing the concurrency model.

User enums, structural records, matching, ownership, constants,
transport and real Option/Result are executable. Generator Step and channel
SendError/ReceiveError now use the same source enum machinery and their focused
execution/transport checks pass. TaskOutcome/TaskFailure, ViewError and LoadError
remain to migrate. S15 must be rerun on the final state: the last broad run's root
suites passed; its native-contract fixture failures were fixed and all 59 IR unit
tests then passed. Current targeted logs are in /private/tmp/enums-status-*.log.
Next: replace the rejected source-type/native-representation boundary before
finishing JoinResult/TaskFailure. Preserve typed causes and payload ownership.

### 2026-11-16 — S05 representation decisions

Enum and variant identities will be declaration-backed typed IDs, retained in
resolved contracts and operations. Generic instances retain concrete type
arguments; recursive contracts use finite nominal backreferences. Constructors
are ordinary callable declarations whose bodies lower to typed enum construction.
Patterns resolve to the same variant declarations, including aliases/reexports.

For the later std migration, existing native Option/Result Value encodings may
remain optimized representations, but their variant/payload contracts must come
from real annotated enum declarations. General construction and pattern checking
must consume those declarations; source names cannot select runtime behavior.

### 2026-11-16 — S05 verified; S06 started

Added declaration-backed EnumId/VariantId metadata, resolved enum contracts,
ordinary constructor declarations, unit-variant values, alias/reexport identity,
and finite recursive enum backreferences. Constructor bodies retain variant IDs
and typed result contracts. Private declarations stay inaccessible. Calling a unit
variant as a function has a typed diagnostic. `#[since]` no longer gets mistaken
for a privileged internal ABI annotation.

Verification: `cargo test -p cyallo-ir --test enum_resolution --quiet` passed five
checks covering identities, imports/reexports, nominal distinctions, constructor
contracts, unit misuse and recursive layout rules. The first compile exposed
missing visitors for EnumConstruct; those paths now retain the payload/type.
One editing script failed before execution because of a duplicate JavaScript
binding; it was corrected and rerun. Enum execution remains gated until S08.

Next: specialize enum templates in their defining modules and infer constructor
arguments from payloads and expected enum result types, including unit variants.

### 2026-11-16 — S06 checked; S07 executable; S08 started

Added generic enum specialization in the defining module, with payload inference
and expected-type constructor arguments (including unit variants). Extended
checked-module dependency traversal to enum contracts/payloads. Unified the
single-AST specialization entry with the module specialization implementation.
Eight enum identity/specialization tests pass; more inference contexts remain to
be checked during matching/standard migration before S06 is marked complete.

Anonymous records now resolve structural types, infer canonical field layout,
evaluate literal fields in source order, lower to aggregate storage and support
field reads/writes and cloning. Ownership, borrowing, cloneability, transferability
and GC metadata traverse their fields. Constants rewrite/pooling retains them.
Two focused source tests pass across eager/deferred checking, interpreter,
Cranelift, Cranelift speed and Winch; nominal coercions, mismatched fields and
borrowed-closure escapes are rejected. Initial checks caught a Rust or-pattern
parenthesis and a remaining constant-rewriter gate; both were fixed and rerun.

Next: introduce typed enum operations and runtime payload storage. Do not claim
S07/S08 complete until moving GC, handoffs and lifecycle checks pass.

### 2026-11-16 — S08 first executable checkpoint

Added typed EnumConstruct/EnumTest/EnumPayload IR operations carrying VariantId.
Enum register snapshots and heap payload cells are distinct runtime encodings;
heap projections can expose real payload addresses for subsequent borrowed
matching. Stores validate the checked variant/payload layout. Snapshot, clone,
move clearing, drop, GC tracing/relocation, lookup fingerprints, native fallback
metadata and constant freezing traverse enum payloads. Detached transport has a
typed enum payload rather than copying Session cell indices (source transport
coverage is still pending). Read concurrency requirements before this addition.

Two source execution tests pass across eager/deferred, interpreter, Winch and both
Cranelift tiers, default/stress GC and alternating one-instruction handoffs.
These cover generic construction, constructor values, constants, clone/equality,
structural records, and managed roots. First checks found missing enum cases in
lookup fingerprints and a nonexistent constant-error variant; added concrete
handling. Execution tests found contextual unit constructors in equality lacked
inference; sibling operand and ordinary argument context now supply it. A test
used an invalid `gc new` spelling and was corrected to the existing `gc value`
syntax. A platform-specific sed invocation failed without editing and was replaced
with a patch. A Rust helper visibility error was corrected and tests rerun.

S08 remains active: lifecycle/payload projection verification and matching still
need coverage. No standard Option/Result migration is claimed.

### 2026-11-16 — S09/S10 execution checkpoint; ownership verification started

Added resolved variant patterns and arm-local renamed bindings. The checker
validates payload/tuple/record/literal patterns, alternative binding agreement,
bool guards, arm result types and recursive pattern-matrix exhaustiveness.
Guarded arms do not establish coverage. Foreign open enums require a wildcard.
`if let` resolves through the same match tree. Block locals remain visible to
tails, and nested returns retain the enclosing routine. Inference/collection and
native adapters now traverse pattern scopes and match block locals.

Lowering evaluates the scrutinee once, tests alternatives in order, projects real
payload cells, aliases guard bindings without copying owners, commits moves only
after guard success, and drops remaining parts/arm locals. An impossible checked
match path has an explicit invariant trap. Ownership analysis merges arm states
and retains projected borrow provenance. Guard moves/mutations are rejected.

Verified the first nested-pattern source test through eager/deferred checking,
all four engines, moving GC stress and one-instruction handoffs. It covers aliases,
alternatives, nested bool/tuple coverage, record rest patterns, guards, block tails,
shared-reference matching and if-let. Initial compilation found missing Lower
initializers, owned-inference String conversions and inner-doc module ordering;
fixed these. Generic record-payload inference was absent; added structural field
unification and reran successfully.

The broad checkpoint stopped at the old transitional frontend test, which still
expected newly executable enums to be rejected (and printed a very large Program
on failure). Removed those obsolete cases. Broad checks will be rerun after the
negative/lifecycle cases. Standard enums are still pending; work is not complete.

### 2026-11-16 — S11 focused ownership/lifecycle checks pass

Five source tests now pass through the eager/deferred, engine, moving-GC and
single-instruction matrix. Added borrowed mutable payload writes against original
storage, RC owner replacement, failed guards with side effects exactly once,
partial payload moves and cleanup of unselected resource parts. Fifteen invalid
source programs reject missing/nested coverage, guards without fallback,
alternative binding/type mismatch, foreign variants, missing payloads, arm/guard
type mismatch, use after move, guard moves/mutations, borrowed result escapes and
out-of-scope bindings. Constructor purity follows the resolved payload lifecycle:
a resource-bearing constructor is a proc, preserving the existing fn/proc rule.

The first lifecycle test exposed a void call at a block tail being lowered as a
value call. Void match results now use ordinary statement-call lowering. Reran
all five tests successfully. Replaced the obsolete unsupported-feature frontend
test with checks that unused enum/structural declarations remain checked in both
modes. Next broad regression check runs while standard enum integration begins.

### 2026-11-16 — Standard Option/Result execute through enum machinery

Replaced the opaque std records with source enum declarations and explicit
Some/None/Ok/Err payload contracts. Private, validated variant annotations select
optimized native encodings; public names never select behavior. Native declaration
metadata retains the actual source enums/variant IDs. Common contract instantiation,
pattern checking, coverage, construction and typed IR consume those declarations.
Legacy some/none/ok/err convenience calls now lower to the same EnumConstruct
operation. Existing convenience methods and native producers keep their ABI.

Borrowed matching of optimized Option/Result projects rooted payload cells and
preserves mutations, copy/move clearing and lifecycle handling. Runtime snapshots
return the optimized ABI encoding for existing native methods. The representation
is separate from declaration identity; it is not a tag-field record or a parallel
list of public variants/signatures. Native enum schemas reject wrong payloads,
variant roles, arity and duplicate encoding declarations.

All six enum source tests pass across the complete focused matrix. The new case
checks standard constructor aliases, structural Result errors, shared/mutable
matching, std parse and collection-pop results, legacy convenience construction,
and owned RC payloads. Its first run used the nonexistent std::str path; corrected
the test to std::num. A missing VariantType encoding initializer was fixed during
compilation. The full workspace regression checkpoint before this std migration
completed successfully; a new full run is required for the final state.

Next: close remaining core syntax/lookup/constant coverage gaps, add runnable
examples, run standard ABI/alias/schema tests and full workspace/lint checks.
Other standard status/error migrations remain separately tracked in S13.

### 2026-11-16 — Remaining core lookup/syntax/constant gaps checked

Added variant brace construction for anonymous and declared record payloads,
including inference through generic declared-record fields. The dedicated
VariantPayload AST preserves the sugar's provenance; ordinary anonymous records
still cannot coerce to nominal records. Added enum variant glob imports, type-alias
variant lookup, contextual generic constructor function values and expected types
inside managed construction. Constant processing carries an explicit evaluation
context into match blocks, evaluates local constants and retains block tails.
Standard enum constants freeze back into the same typed constructor nodes.

Verified brace syntax, aliases/globs, constructor values and block constants in the
full focused engine/GC/handoff matrix. Verified native standard variant renaming
without compiler changes and rejection of malformed ABI variant/payload schemas.
Foreign open-enum wildcard rejection and same-package exhaustiveness pass.

Two additional source matrix tests pass: recursive generic managed enum lists plus
standard enum constants; and enum trait dispatch, owned closure captures, matching
inside closures and dynamic round-trips. The recursion test exposed two real gaps:
managed construction lost expected types, and a resolved enum and its backreference
compared differently despite sharing declaration/type-argument identity. Fixed
context propagation and nominal identity comparison. Also fixed source rendering
of nested specialized names. Brace resolution initially attempted to resolve an
already-resolved internal constructor symbol; now it resolves the source operands
once and retains the selected declaration. All affected tests were rerun.

The standard regression batch (native declarations, modules, managed storage,
composites, enum tests and checked-module units) passed before these last gap fixes.
Next: runnable examples, broader negative/capture/transport checks and final lint /
workspace verification. S13 remains the separate migration of other std statuses.

### 2026-11-17 — Examples and expanded engine verification

Added four runnable enum examples (Result errors, patterns, ownership and recursive
managed lists) and CLI coverage for all four engine flags with GC stress. Added
heap-independent task/channel enum transport coverage and a Cyallo concurrency
example; local managed/borrowed enum payload transport is rejected. The focused
transport test passed with the existing concurrency engine/checking matrix.

The example matrix exposed a native handoff bug for a borrowed numeric payload:
its address register was typed as the pointee scalar when stored into a reference
binding. Winch's scalar store then treated it as an integer. Lowering now creates
an explicit reference-typed register, as ordinary borrow expressions do. All 11
then-current enum tests passed after this fix. Extended the matrix to continuous
native execution as well as handoffs, and to compare instruction counts.

Added internal bottom types for empty enums and diverging match arms. Added
literal and structural pattern checks, borrowed closure escape rejection, native
schema rejection of opaque Option/Result records, and annotation-order coverage.
Private semantic annotation lookup now ignores version metadata in source/native
headers and type declarations. The 12 language tests and 14 native declaration
tests passed at that checkpoint. Clippy found one collapsible match and three
err/expect test calls; corrected them and reran with warnings denied successfully.

Unused generic enum templates now validate value recursion and generic arity before
template extraction. Added coverage through generic record wrappers and managed
indirection; finite nested instantiations remain valid. Two old metadata tests
were bypassing specialization (std enums now have real generic constructors);
updated them to exercise the real pipeline. All nine resolver/specializer tests
pass. Removed obsolete source-feature rejection categories; unresolved parsed
if-let nodes now report that they have not passed resolution. Anonymous record
literal borrows also use the ordinary aggregate temporary path and account for
owned fields during borrowing.

A few patch applications failed harmlessly after formatting changed their context;
reapplied against the current text. No partially applied edits were retained.
Next: final workspace/all-feature tests, format, Clippy and vendor checks; update
this log and the tracker from their results. Other std status migration is S13.

### 2026-11-17 — Full-suite stack regression diagnosed and fixed

The first final all-feature run reached `tests/nesting.rs` and aborted with a
2 MiB worker-stack overflow. Used the diagnosing-bugs skill with the existing
small-stack test as the red feedback loop. Minimized it to compiling one binding
with 200 parentheses: no execution or print call is required. Parsing, cloning
and inference in isolation passed; 100/150 nesting levels compiled, while 200
failed repeatedly in under a second. No glossary/ADR changes were needed.

Ranked constructor-context recursion first, unguarded cloning second, pattern
traversal third. LLDB could not start its child in this environment (`no such
process`), so tested the first prediction directly with one code change. Adding
the existing per-level with_stack guard to context_expression made the minimal
repro and original nesting suite pass. This was the actual regression: real std
constructors now cause specialization/context traversal in ordinary programs, and
the new recursive traversal lacked the stack contract used by other AST passes.

Applied the same guard to the other new recursive pattern/specialization walkers.
Added a permanent nested enum/tuple-pattern/block compilation test on a 2 MiB
worker stack. All four nesting tests pass. Removed the throwaway probe file;
there are no debug logging statements to retain. Final workspace verification must
be rerun after this fix.

### 2026-11-17 — Native-contract fixture fixed; S13 status migration starts

The next full run passed the root language/runtime suites, then found 22 native
contract unit failures sharing one fixture. That fixture resolved composites
before specializing the now-real generic std constructors. Updated it to use the
normal specialization stage; all 59 IR unit tests pass. This is a fixture pipeline
fix, not a compiler exception for standard enum constructors.

Continuing S13 with generator Step and channel SendError/ReceiveError. They will
retain their typed native encodings and existing methods while their variants and
payload contracts come from actual source enums through the common checker.
Generalize native enum argument/schema handling before adding these declarations;
constants, borrowed projections and native producers must use the same contracts.
TaskOutcome/TaskFailure, ViewError and LoadError remain subsequent S13 work.

### 2026-11-17 — Step and channel error enum migration verified

Generator Step now declares Yielded/Complete; SendError declares Closed/Full/Limit
with an owned T in each; ReceiveError declares Closed/Empty. Private role
annotations validate their source schemas. Common native enum argument handling
supports zero, one and multiple parameters. Construction, matching, constants and
borrowed payload storage use the shared enum operations. Native enum codecs live
in runtime/enums.rs. Optimized native storage compares validated ABI roles across
published declaration-ID relocations; user enums retain nominal declaration IDs.

Moved the shared generic GeneratorStep carrier into the heap-independent payload
crate (cyallo-values reexports it). Detached transport retains explicit typed
Step/SendError/ReceiveError wrappers and owned payloads. Source transferable
checking traverses the native source enum contracts; no Session cell IDs cross
Sessions. The focused source test passes in the full eager/deferred, engine, GC,
continuous/handoff and instruction-count matrix. It covers constants, native
producer results, mutable yielded payloads and RC ownership recovery. All ten
existing generator tests pass. Two source concurrency cases pass for cross-session
status transport and the new status-enums.ya example.

The first source test incorrectly attempted a channel of local RC values, which
the existing locality rule rejects. Corrected it to test RC recovery through a
constructed error and native send failures through a transferable string channel.
Transport testing exposed missing constructor context in member-call arguments;
the specializer now obtains expected arguments from resolved native member
contracts. This also supports cases such as sender.send(Option::None) without
hardcoded member names. Reran the transport tests successfully.

### 2026-11-17 — Task result/failure representation and naming decisions

Public names will be TaskHandle<T> (the cancellable/joinable handle) and
TaskResult<T> (Completed/Failed/Cancelled/Stopped), with Task and TaskOutcome kept
as source aliases. A completed language Result::Err remains Completed(Err(...)).

TaskFailure must preserve Guest, Panic, Destructor and Cleanup causes. Host error
objects and panic payloads stay typed opaque payloads, not formatted error strings.
Cleanup retains its two failure values behind explicit managed indirection.
For these richer statuses use a typed native enum snapshot retaining ordinary VM
payload values. Do not reconstruct Rust failures from GC payloads, which would
lose source payload identity and ownership. Native task results normalize into
these snapshots when a parked join resumes, with roots across any allocations.
Common enum tests/projections/clone/drop/GC still handle the payloads. Formatting
is a separate convenience operation. Keep migration unfinished until source
programs and typed-cause checks execute through the VM.

### 2026-11-17 — User rejected the builtin type catalogue architecture

The user corrected the join return type name to JoinResult, then explicitly
rejected the StorageKind representation of standard-library types, including
collections, enum statuses, generators and capabilities. A rename or relocating
the same catalogue is not a solution. Source declarations must own names,
nominal identity, fields/variants and callable contracts. Native representation
and coroutine mechanisms belong below that boundary; optimization must consume
resolved declarations, traits and layouts rather than replace them.

The Generator/Poll comparison illustrated that distinction. Poll is not an
existing feature or a requested addition; keep the stackful coroutine model.
Do not introduce Future or a polling API as part of this correction.

The in-progress TaskFailure/native enum snapshot patch is not verified and the
worktree does not currently compile. Its proposed TaskOutcome/TaskFailure
StorageKind additions and dedicated NativeEnum snapshot are superseded by this
correction. Earlier verification applies to the preceding Step/channel checkpoint,
not this unfinished patch. Resume by replacing the representation boundary;
do not complete or advertise the rejected per-standard-type implementation.

### 2026-11-17 — Pending work handed back to the user

Inventoried the unfinished patch and earlier verification logs, wrote the ordered
pending-work.md handoff, and reopened S12 because the standard enum type markers
remain. Ran a fresh workspace/all-feature cargo check: it fails on missing task
enum encoding handling in runtime/enums.rs and missing NativeEnum/HostError/
PanicPayload handling in runtime/lookup.rs. The log is
/private/tmp/enums-handoff-check.log. No implementation was resumed during this
handoff. The current branch has not passed final verification.

### 2026-11-17 — Direction changed to SDKs/worlds; phase 0 baseline restored

The user rejected the StorageKind/EnumEncoding model and approved a replacement plan:
independent runtime SDKs (`core`, `collections`, `tasks`, …) with a `prelude` SDK and an
init world; pure code with bodies in the SDK, host-only items bodyless and bound by a
per-Runtime table; engine-owned intrinsics plus embedder-defined intrinsics with
reference semantics, safe emitters and a conformance harness; a generic enum layout
optimizer. Plan: `/Users/cyan/.claude/plans/hashed-hopping-lecun.md`.

Phase 0 done: removed the unfinished TaskFailure/JoinResult additions (the eight
Task*/Failure* `EnumEncoding` cases and keys, `StorageKind::TaskFailure/HostError/
PanicPayload`, `Value::HostError/PanicPayload`, the concurrent-values HostError/
PanicPayload re-exports, their member intrinsics) and restored `std/task.ya` to the
pre-patch `Task`/`TaskOutcome` records. Added the missing `Value::NativeEnum` arm to
`lookup_fingerprint` (a temporary arm; `NativeEnum` itself is slated for deletion).
`cargo check --workspace --all-features --tests` passes. Nothing else verified: no
tests were run, and Clippy/format/vendor checks are not rerun. Remaining `StorageKind`/
`EnumEncoding`/`Value::NativeEnum` machinery is the rejected model, kept only until
phases 1-8 replace it.

### 2026-11-17 — Phase 1a: SDK/world syntax and AST

Added general (not library-specific) syntax: `interface mod` (`ModuleDeclaration.interface`),
`opaque record Name<T>;` (`Record.opaque`; braces rejected), `world` with `include`,
`import path [as alias] [{ signatures }]` and `export` signatures (`Ast.worlds`), and
`impl interface P for M { type T = U; }` (`Ast.interface_implementations`). The words
stay contextual identifiers elsewhere. Parallel parse-unit assembly and span relocation
cover the new nodes. New `crates/cyallo-parser/tests/sdk_worlds.rs` (4 tests: AST shape,
sequential vs parallel equality, five malformed forms, contextual identifiers) pass, as
do the parser and AST crate tests; the workspace compiles with `--all-features --tests`.
Not done: name resolution and declaration identities for these nodes (phase 1b), so
nothing consumes them yet; no span-relocation test for worlds; full workspace tests,
Clippy and format not rerun.

### 2026-11-17 — Phase 1b: interface/opaque resolution

`namespace.rs` now records `interface mod` nodes and source `opaque record` declaration
ids. `impl interface P for M { type T = U }` is resolved after declarations: P must be an
`interface mod`, M a module, T an opaque record of P. Each opaque record is bound exactly
once across the program; if any implementation of P exists, every opaque record of P must
be bound; bound types resolve in the declaring module's scope and reuse the type-alias
path (so cycles are rejected). Generic opaque records cannot be bound yet (typed error;
needs generic alias support). Bodyless functions are accepted inside `interface mod`
(contract items) and remain rejected elsewhere without a native annotation. Record
literals of an opaque record are rejected (`OpaqueRecordConstruction`). New typed
`DeclarationError` variants carry the names. New `crates/cyallo-ir/tests/sdk_interfaces.rs`
(5 tests) passes; parser/ast/ir unit tests and `native_declarations` (14) and
`data_enums` (13) pass.

Open: `crates/cyallo-ir/tests/enum_resolution.rs::payload_contracts_are_nominal_and_
constructor_bodies_keep_variant_ids` fails because it also iterates core enum
constructors, whose result is `Storage(ReceiveError, Void)`, not `Type::Enum`. This code
path is untouched by phases 0-1b, but I did not confirm the test passed before. It is
part of the rejected StorageKind model; revisit when that is removed. Not done in
phase 1: identities across SDK major versions, `#[since]` rules, world resolution
(include/import/export), prelude SDK. Clippy/format/full workspace tests not rerun.

### 2026-11-17 — Phase 1c: world resolution

`world` is a new `Kind::World` symbol (excluded from dynamic-module exports), resolved
after interface bindings. `ResolvedProgram.worlds` holds `ResolvedWorld` values: `include`
expanded (cycles and self-inclusion rejected; a diamond keeps one copy by origin), imports
with the generated-environment field name (alias or last path segment; duplicates rejected
unless identical via includes), optional member lists, and exports with resolved
signatures. A member list is checked against the real module: the routine must be
accessible and have the same kind, parameter passing/arity/types and result (generic
parameter names are matched positionally). Worlds are part of the cache identity program.
New typed `DeclarationError` variants. `crates/cyallo-ir/tests/sdk_worlds.rs` (4 tests)
passes; so do sdk_interfaces (5), parser/ast/ir unit tests, `native_declarations`, `data_enums`,
`cli`, `dynamic_import` and `module_units`. The `enum_resolution` failure noted above remains.
Not done: `W::Env`/`W::Exports`/`W::load` generation and instantiation checks (phase 3),
exports checked against an implementing component, `#[since]`/digests (phase 2), the
prelude SDK. Clippy/format/full workspace run not repeated.

### 2026-11-17 — Phase 2a: interface digests and `#[since]` rules

New `crates/cyallo-ir/src/sdk.rs` (uses `sha2`): `Version` (strict `major.minor`), `Digest`
(SHA-256 over a canonical text), `InterfaceItem`, `InterfaceDigests`, typed `SdkError`
and `check_minor_update(previous, next, version)` (carried-over items byte-identical,
nothing removed, new items marked with exactly this release, no item newer than it,
worlds not removed). The resolver builds the canonical forms: type spelling is replaced by
the declaring module path plus source name, relative to the item's package, so aliases and
reexports do not change a digest but a different declaration does; parameter names,
comments, formatting and declaration order are ignored; opaque records digest as identity
and generic arity. Covered: public functions, records, opaque records, enums (with
per-variant `#[since]` and openness), traits, type aliases and methods declared in
`interface mod` impl blocks (contract-only, no native annotation needed). World digests
cover imports (alias, module, member signatures) and exports, in sorted order.
`ResolvedProgram.interface` carries both. Malformed `#[since]` is rejected at link time.
Tests: `sdk.rs` unit (3) and `crates/cyallo-ir/tests/sdk_digests.rs` (5) pass; earlier suites
unchanged (same `enum_resolution` failure as before). Not done: digest of a whole-module
import, use-set/lockfile and manifest ranges, `#[since]` use-site checks against a
declared minimum (needs manifests), the implementation table and contract validation (2b),
the intrinsic catalogue audit (2c). Clippy/format/full workspace not rerun.

### 2026-11-17 — Phase 2b (part 1): intrinsic contract table

New `crates/cyallo-ir/src/contract.rs`: `Intrinsic::contract()` returns an
`IntrinsicContract` for all 100 intrinsics from a wildcard-free match (adding an intrinsic
forces a decision): `Disposition` (engine primitive 70 / host call 8 / source candidate
22), `Safepoint` (never / may collect / may re-enter), `Effects` (reads, writes,
allocates, fresh identity, nondeterministic, io, may fail, may park, calls guest;
`is_pure()`), and argument consumption. `inlinable()` is the gate a backend would use.
`Intrinsic::consumes_argument` now delegates to the table; tests keep the old list as an
oracle (identical for all 100), check that contract purity is never weaker than the old
`builtins::pure` rule (13 documented lookup-lowered exceptions), check internal
consistency (parking/guest calls imply re-entry, allocation implies a safepoint, source
candidates are pure), and pin the audited partition. 29 contract-related tests pass; the
workspace compiles; `native_declarations`, `data_enums`, `cli` pass; the earlier
`enum_resolution` failure is unchanged.
Not done: nothing consults `effects`/`safepoint` yet (`gc_metadata`, `effects.rs` and
`builtins::pure` are unchanged), `MemberIntrinsic` (75) has no contract table, `may_fail`
is conservative for scalar ops, and the registration API for host calls/embedder
intrinsics is still to build.

### 2026-11-17 — Phase 2b (part 2): safepoints consult the contract

`GcMetadata::build` now derives `Op::Builtin` safepoints from `Intrinsic::contract()`:
a builtin whose contract is `Safepoint::Never` (scalar casts and math, lengths, char
conversions, substring tests, Count/Sum, Random) is no longer marked `may_collect`/
`may_reenter`; builtins that allocate keep the conservative flags. Backends, OSR liveness
and the post-helper validity check read these flags, so non-collecting builtins skip
collection checkpoints and the re-entry check; the inliner still handles only generic
ops, so no instruction selection changed. New unit test in `gc_metadata.rs`.
Verification: full root `cargo test --no-fail-fast` produces exactly the same 15 failing
tests with and without this change (names identical; only timings differ), so it adds no
failures, and the gc/engines/native/concurrency/data_enums suites pass. Those 15
failures (attributes, effects, eager, deferred, constants, inlining, tiering/OSR/native
callee tests) pass in the original workspace and fail in this worktree: the migrated
std enums (e.g. `ReceiveError`'s `Closed`/`Empty` constructors) now appear as extra
functions ahead of user code, shifting the fixed `functions()[1]`/`[2]` indices these
tests assume. Same root cause as the `enum_resolution` failure; both belong to the
rejected StorageKind model and should clear when it is removed (to be confirmed then,
not assumed). Not done: `effects.rs`/`builtins::pure` still use their own rules; no
backend inlines intrinsics yet.

### 2026-11-17 — Phase 2b (part 3): effects consult the contract

`effects.rs` derives `Op::Builtin` effects from `Intrinsic::contract()`: `may_fail` is the
contract's `may_fail` or allocation (resource exhaustion), `may_allocate` adds the
contract's allocation to the existing result-type rule, `io` comes from the contract, and
a non-pure contract is the purity violation. `builtins::pure` now answers from the
contract (same answers for every intrinsic that lowers to `Op::Builtin`; the 13 stricter
ones lower through `Op::Lookup`). `Random` is marked fallible to keep its old effect.
Effect: a zero-local `fn` calling a total scalar builtin (e.g. `std::char::code`) is no
longer `may_fail`/`may_allocate`; allocating or overflow-capable builtins stay fallible.
New `tests/intrinsic_effects.rs` (2 tests) passes. The full root suite fails exactly the
same 15 tests as before (names compared), cyallo-ir/parser/ast tests have only the known
`enum_resolution` failure. Note: every function with locals still starts `may_allocate`/
`may_fail` (`effects.rs` initialization), which masks builtin precision there.
Not done: `lookup::Operation::writes`, `ownership.rs` intrinsic match, `MemberIntrinsic`
contracts, backend inlining.

### 2026-11-17 — Phase 2b (part 4): member contracts

`MemberIntrinsic` (75 variants; now generated by a macro with `ALL`) has
`MemberIntrinsic::contract()` returning a `MemberContract`: disposition (34 engine / 12 host /
29 source), safepoint, the shared `Effects` record, and per-operand passing
(`OperandModes`: receiver, rest, positional exceptions). `declarations.rs::operand_mode`
now delegates to it; a test keeps the previous hand-written table as an oracle and checks
all 75 members at operand positions 0-4. Further tests check internal consistency
(parking/guest calls imply re-entry, allocation implies a safepoint, source candidates are
pure) and pin the partition and the inlinable set. `docs/intrinsic-contracts.md` gained a
member section and tables. 32 contract tests and 71 cyallo-ir unit tests pass; the root
suite fails the same 15 tests as before (names compared). Not done: effects/GC metadata
for `Op::Lookup`/`Op::Service` (where members lower) do not read member contracts yet,
and `ownership.rs`/`lookup::Operation::writes` still keep their own rules.

### 2026-11-17 — Examples: `examples/sdk/`

Added runnable `worlds.ya`, `unbound.ya` (rejected) and `prelude.ya`, with a README and an
index entry. `std/prelude.ya` now exposes `prelude` and a `prelude::core` group
(Option/Some/None, Result/Ok/Err) as plain reexports, so `use prelude::core::*` works
today. The variant-rename test in `tests/native_declarations.rs` also edits the prelude
reexports. Verified on the interpreter and `cranelift_speed` with GC stress; the
native_declarations, data_enums, cli, dynamic_import and module_units suites pass. The
real per-SDK split of `std` (and `prelude::collections`/`tasks`) is still phase 6.

### 2026-11-17 — Phase 2b (part 5): lookup operations use contracts

`lookup::Operation::contract()` maps every lowered lookup operation (wildcard-free) to the
contract of the intrinsic/member it implements; seven operations without a single origin
get `OperationContract::CONSERVATIVE`. `gc_metadata.rs` now derives `Op::Lookup` safepoints
from it (a `Never` lookup is neither `may_collect` nor `may_reenter`), and `effects.rs`
derives `may_allocate`/`may_fail` for `Op::Lookup` from it while keeping its existing
special cases (concurrency, ref-counting, drop/clone lifecycle purity violations). A debug
assertion in `src/runtime.rs` checks that a `Never` lookup allocates no heap cell
(collection only starts at allocation). Verification: the full root suite ran with the
assertion and it never fired; it fails exactly the same 15 tests as before (names
compared); 72 cyallo-ir unit tests pass including a new test that checks origin agreement
and the conservative set. Limits: the assertion covers cell allocation only; `Op::Service`,
`Op::Dynamic`, `lookup::Operation::writes` and `ownership.rs` do not read contracts yet;
no backend inlines intrinsics. A wrong `Never` label would be unsound; the assertion is
the guard, so keep running the suite in debug when changing contracts.

### 2026-11-17 — Phase 2b (part 6): registration API

New `crates/cyallo-ir/src/registry.rs`: a per-instance `Registry` collecting
`host_call`, `intrinsic` (with optional reference semantics and declared lowerings) and
`opaque_type` registrations against SDK item paths and digests; `install(&InterfaceDigests)`
validates them all at once into an immutable `Installed` (contracts queryable by path).
Checks: unknown item, not bindable, kind mismatch, digest mismatch (names both digests),
duplicate, unbound bindable items, and contract rules (parking/guest calls need a
re-entry safepoint, allocation cannot be `Never`, host-call and intrinsic dispositions,
missing reference, operand exceptions within arity). Supporting change:
`InterfaceItem` records `requires_binding` (bodyless routine/method, opaque record) and
`arity`, neither part of the digest. `crates/cyallo-ir/tests/registry.rs` (8 tests) passes
with the existing suites (72 unit tests; only the known `enum_resolution` failure).
Documented in `docs/sdk-registration.md`. Not done: nothing consumes `Installed` yet,
references are names only (no conformance harness), no emitter API for lowerings, no
load handshake. Root suite not rerun for this change (no root-crate code changed).

### 2026-11-17 — Phase 6 (first slice): the compiler consumes provider bindings

New `NativeBindings` (SDK item path → engine key) carried by `CorePackage`
(`with_bindings`; embedded package uses `NativeBindings::embedded()`). The resolver applies
it to Core-trust source in `add()` as the private annotation the pipeline already reads, so
no downstream stage changed; user source never receives bindings. Typed errors:
`BindingConflict`, `UnusedBinding` (checked after the core package is declared). The `Cell`
family (type, `new`, `replace`) moved out of `std/cell.ya`, which now has no annotations.
`tests/native_bindings.rs` (6 tests) covers provider-bound Cell running on the
interpreter, absence of bindings, renaming a declaration by editing only the provider table
(stale binding is a typed error), annotation/binding conflicts, unknown key and wrong kind,
and user source never being bound. Other custom `CorePackage` builders (`src/dynamic_modules.rs`,
two test helpers) now pass `NativeBindings::embedded()`. Full root suite: the same 15
failures as before (names compared). `cyallo-ir`'s `standard::tests::every_bundled_asset_is_actual_cyallo_source` failed
after the Cell move (it required an annotation on every native callable); it now accepts a
provider binding in place of the annotation and checks every embedded binding's key. With that
fixed, cyallo-ir/parser/ast show only the known `enum_resolution` failure. (An earlier version of
this entry omitted that failure.) Limits: keys still map to `StorageKind`/`abi_keys!`; `Registry`
is not yet used by the linker (needs std modules as `interface mod`s with digests); only
`Cell` is migrated; a host-provided `CorePackage` that copies embedded sources must also
pass the bindings.

### 2026-11-17 — Phase 6 (second slice): every std family bound by the provider

All 227 native declarations of `std` (types, functions, methods, enum variants, traits) moved
from `#[cyallo_internal]` annotations in `std/*.ya` to `crates/cyallo-ir/bindings/std.bindings`,
parsed by `NativeBindings::embedded()`. Generated by a script from the existing annotations
(same keys, so every pipeline stage after the resolver is unchanged); only the `prelude`
module annotation remains in source. Methods are addressed by owner: nominal receivers by
name, others (`[]T`, `rc T`, `gc T`) by spelling. `NativeBindings::map` added. Fixes found on
the way: runtime-loaded modules now link against the host package's bindings
(`src/dynamic_modules.rs` was passing the embedded table); the bundled-assets unit test accepts
provider bindings. Tests that rewrote std source text now edit the table the same way
(native_declarations: move/rename task, unbind spawn, key swap, variant renames, role swap,
version annotations; dynamic_import: move `std::module`). Verification: full root suite fails
exactly the same 15 tests as before (names compared); cyallo-ir/modules/parser/ast/values
show only the known `enum_resolution` failure; `native_bindings` (6) pass. Limits: keys still
map to `StorageKind`/`abi_keys!`; module annotations (`prelude`) are not bindable; the registry,
digests and contracts are still not consulted by the linker.

### 2026-11-17 — StorageKind removal, first family: Clock

`StorageKind::Clock` deleted. New `Type::Opaque(target, arguments, OpaqueTraits)` in the AST: a
host type known only by its declaration plus provider-declared traits (copy/transfer/effectful/
clone), parsed from binding keys `opaque[:flags]` (`std::time::Clock opaque:transfer,effectful,clone`).
The resolver maps a bound opaque record to it (declared arity checked); ownership, transfer,
cloneability, borrows, substitution, unification and walking handle it; the clock constructor's
result type comes from the declared signature and the clone/sleep members apply to any opaque
receiver. Native-contract unit test updated to take the clock type from the declared receiver.
Tests (`tests/native_bindings.rs`, 9 total): renaming the declaration changes nothing else; a user
`Clock` is a different type; traits come from the binding (clone via `std::mem::clone`,
effectfulness, malformed flag). Verification: root suite fails the same 15 tests as before (names
compared), 72 cyallo-ir unit tests pass, `enum_resolution` failure unchanged, clock example passes on
every engine. Limits: 26 variants remain; members of other families still synthesize result types by
kind (`member.rs`), which blocks them; generic opaque arguments are plumbed but unused. Plan:
`docs/storagekind-removal.md`.

### 2026-11-17 — StorageKind removal: Sender and Receiver

`StorageKind::Sender`/`Receiver` deleted; both are generic `Type::Opaque` types bound as
`opaque:transfer,effectful,clone` in `std.bindings`. Channel constructors take their result pair
from the declared signature; channel members apply to any opaque receiver with the element type
taken from the applied argument (`opaque_argument`); the duplicate-canonical-declaration check skips
opaque declarations (they legitimately share traits). Native-contract unit tests build channel ends
from the declared receiver. Tests (`tests/native_bindings.rs`, 11 total): channels run, renaming both
ends in source and table changes nothing else, a user `Sender` is a different type, a channel of
channel ends depends on the declared `transfer` flag. Verification: root suite fails the same 15 tests
as before (names compared); 72 cyallo-ir unit tests pass; channel and task tests pass on every engine
via the existing suite. Limits: `SendError`/`ReceiveError` payloads are still kind-based; 24 variants
remain.

### 2026-11-17 — StorageKind removal: Task and TaskOutcome

`StorageKind::Task`/`TaskOutcome` deleted; bound `opaque:effectful,resource` and `opaque:clone`. New
`resource` trait; `copy()` honours `copyable`; `potentially_owning` for opaque types is `resource || owns-by-arguments`.
`spawn`/`join` results come from the declaration (`member::specialize` now receives the declared result);
shape validation kept in `concurrent.rs` (`opaque_with`, `channel_pair`) and `resolve_declared` (non-opaque declared
result for `clock`/`spawn` is `InvalidSignature`). A first version dropped that validation and made two native
declaration tests fail; both restored (and the archive test's key swap now targets `std::task::Task`). Tests
(`tests/native_bindings.rs`, 13): task runs, handle and outcome renamed in source and table, wrong handle argument
rejected, bool result is E173, handle uniqueness (clone and send rejected, allowed when the table grants the traits).
Verification: root suite fails the same 15 tests as before (names compared); cyallo-ir unit tests pass except the known
`enum_resolution`. Limits: 22 variants remain; `TaskOutcome` still opaque, not a data enum.

### 2026-11-17 — StorageKind removal: ArenaCell, Generator/PureGenerator, Weak (batch)

`StorageKind::ArenaCell`, `Generator`, `PureGenerator` and `Weak` deleted; 18 variants remain (164 uses). New opaque
traits: `indirect` (recursion through the type), `generator` (language-driven coroutine; `generator::parts` and
`factory_type` use it; `pure` is compiler-set and replaces `PureGenerator`, accepted where the general type is
expected), `weak` (weak reference to an `rc` owner: `Weak<'a, T>` surface handled in the resolver, `downgrade`
result from the declaration, `weak_type()` finds the provider's declaration by trait, `WeakOwner`/`Target`/receiver
unification use it). Opaque types compare region-aware in `same_regions` (a missing arm initially broke two managed
tests). Native-contract unit tests build generator and weak types from declared receivers. Tests
(`tests/native_bindings.rs`, 16): recursion through `ArenaCell` needs the declared `indirect`; generators run, renamed
to `Coroutine`, and fail without the `generator` trait; weak references run, renamed to `Ref`, and fail without `weak`.
Verification: root suite fails the same 15 tests as before (names compared) after each of the three migrations;
cyallo-ir unit tests pass except the known `enum_resolution`. Limits: the language still names traits, not types,
for these three; `GeneratorStep`/`Option`/`Result`/error enums remain kind-based until the data-enum step.

### 2026-11-17 — StorageKind removal: Cell, Location, StoragePath

Deleted `StorageKind::Cell`, `Location`, `StoragePath` (15 remain). New traits `deref` (`.*` yields the argument;
inference and assignment analysis use it), `target` (`Target<Owner>` family), `path` (mapper results, path
members). Constructors for cells, paths, missing paths and field locations take their result from the declaration
(`resolve_declared` passes it; `lookup::constructor` and `FieldLocation` validate its shape); path members take
their result from the declaration. Recursion uses `indirect`. Native-contract unit tests build the types from
declared receivers. Tests (`tests/native_bindings.rs`, 18): cell deref needs the declared `deref`; the path program
compiles under the real binding, behaves the same after renaming `StoragePath` to `Route`, and is rejected without
`path`; the wrong-kind test now uses `vec_struct` since `cell_struct` no longer exists (and the conflict test uses an
opaque key). Verification: root suite fails the same 15 tests as before (names compared); cyallo-ir unit tests pass
except the known `enum_resolution`. Limits: `Location` has no semantic trait (any single-argument opaque receiver
matches `load`/`store`; the declared signature is the check).

### 2026-11-17 — StorageKind removal: Allocation and Layout

Deleted `StorageKind::Layout`/`Allocation` (13 remain). Role traits `layout` and `allocation`; the allocator-trait
validation checks roles; `rc(allocator)` lowering takes the layout and result types from the allocator trait's declared
`allocate`; `Layout::new`/`allocate` results come from the declaration with role checks in `memory::resolve`; layout is
copy via its `copy` trait. Test (`tests/native_bindings.rs`, 19): a counting allocator runs, still runs after
renaming `Layout`→`Shape` and `Allocation`→`Block` in source and table, and is rejected without the `layout` role.
Verification: root suite fails the same 15 tests as before (names compared; the managed-memory allocator tests pass on
every engine); cyallo-ir unit tests pass except the known `enum_resolution`. Limits: `AllocError` and `Result` are
still kinds.

### 2026-11-17 — StorageKind removal: Vec and VecDeque

Deleted `StorageKind::Vec`/`VecDeque` (11 remain, 91 uses). Traits `collection` and `double_ended`; `Type::collection_element`
and `is_double_ended` helpers. Literal heads are recognized by the trait and carry the declared type as the first type
argument; `Vec::from` takes its result from the declaration; indexing, `for`, `len`, slices, `ArraySum`/`ArrayLength`,
`Collection<T>` bounds, slice unification, `push`/`pop` and path `element` use the trait; the runtime's three
collection checks use it too. A first run failed three tests (literal operand typing used the first type argument as
the element; now the second; and two of my own tests used keys/syntax that no longer apply). Tests
(`tests/native_bindings.rs`, 21): literals, indexing, push, push_front and `Vec::from` run, still run after renaming
`Vec`→`Seq` and `VecDeque`→`Ring` in source and table; front growth needs `double_ended`; a literal head needs
`collection`. Verification: root suite fails the same 15 tests as before (names compared); cyallo-ir unit tests pass except
the known `enum_resolution`. Benchmarks not run (see `docs/storagekind-removal.md`).

### 2026-11-17 — Benchmarks for the Vec/VecDeque migration

Release builds of the original workspace (`overhaul`) and this worktree, alternating runs, startup subtracted. A
collection workload (push, indexed reads, deque both ends) is 1.020-1.032x the baseline (interpreter and jit, 20k and
80k elements); an integer-only control is 1.016-1.028x; fixed startup is +2.8 ms. The slowdown is general, not
collection-specific, and comes from earlier branch work (not bisected). Output identical in all runs. Criterion benches
and an array workload were not run. Details in `docs/benchmarks.md`.

### 2026-11-17 — Bisecting the 2-3% slowdown

No history to bisect; measured instead. `size_of` showed `Type` 56 (baseline 48), `Value` 72 (64), `Op` 96 (80),
`Instruction` 104 (88). Causes: `Type::EnumRef` held an `EnumIdentity` (with a `Vec`) inline, `Op::EnumPayload` held a
`Type` inline, and `OpaqueTraits` was 16 bools. Fixes: `EnumRef(Box<EnumIdentity>, Box<str>)`, `EnumPayload(.., Box<Type>)`,
`OpaqueTraits(u16)` bitset with getters/`with_*` (rewritten from compiler diagnostics). Sizes are back to the baseline's.
Startup: a profile of in-process compile showed `SipHasher` at about 40% in both trees: `ModuleKey` hashed the normalized
debug text of every native declaration for each module; it now holds SHA-256 digests. Compile 4.1 ms vs baseline 4.7-4.9 ms.
Result (release CLI): startup -0.5 ms, collections 0.990-1.014x, integer control 0.985-1.006x. Full root suite fails the same
15 tests as before (names compared); cyallo-ir unit tests pass except the known `enum_resolution`. Details in
`docs/benchmarks.md`.

### 2026-11-17 — Structural identity for the module cache

Removed every `format!("{:?}")` identity from `module_cache.rs` (AST, native declarations, signatures, global and type
contracts, service implementations, permissions). `Hash` is derived across `cyallo-ast` and the `services` types;
`Literal` and `ConstantValue` implement it by hand (reals by bit pattern); `Ast.source_names` and
`NativeImportPolicy.requesters` are `BTreeMap` (deterministic, hashable; the CAIE parser converts its map). A `Hasher`
adapter streams the structure into SHA-256. Unit tests: equal source gives equal identity, any structural or literal change
differs, `0.0` vs `-0.0` differ, map insertion order does not matter, length prefixes keep concatenations apart.
Full root suite fails the same 15 tests as before (names compared); cyallo-ir unit tests pass except the known
`enum_resolution`. Compile 3.7 ms (was 4.1 with digested text, 4.9 baseline).

### 2026-11-17 — Persistent compile workers

`cyallo-compile-jobs` no longer spawns threads per stage. `Jobs::map`/`for_each_mut` run on a `rayon_core` pool kept per
configured width (`Jobs::workers()`, not per batch size), named `cyallo-compile-N`, created on first use and never torn
down. Semantics kept: results published in input order, panics reach the caller, nested stages get `Jobs::SEQUENTIAL` and
run on their worker, borrowed data allowed. New tests: workers persist across 20 stages, batch size creates no pools, a job
panic reaches the caller and the pool survives, mutable items, six concurrent callers share the pool. Full root suite fails the
same 15 tests as before (names compared). Compile 3.2 ms warm (was 3.7), CLI startup 7.7 ms vs 8.8 baseline.
Design note: this is a process-wide pool, which sits uneasily with the rule against process-wide singleton services; it holds
capacity only (no registry, ids or program state), and compile entry points have no owner to hold it. A runtime- or
session-owned pool would need an explicit handle in `Jobs`/`CompileOptions`.

Decision (user, 2026-11-17): keep the compile worker pool process-wide. Recorded as the single sanctioned exception in
`docs/compiler-and-runtime-rules.md` (capacity only; no identifiers, registry or program state; not to be extended).

### 2026-11-17 — ReceiveError as a data enum

First `StorageKind` data enum removed. See "ReceiveError: first data enum" in `docs/storagekind-removal.md`. Role table
(`variant_roles`) lives in `NativeDeclarations` and `Program`; runtime produces general `Value::Enum`. Full suite: same 15
baseline failures, no new ones.

### 2026-11-17 — Option/Result values as core enums

Reserved variant ids; dedicated `Value`/`Payload` variants removed; helper constructors and views on `Value`. Same 15
baseline failures. Types still use the `Storage` marker (next step).

### 2026-11-17 — Step/Send/Receive values as core enums

Reserved ids 4-10, dedicated values and payloads removed, role table deleted. Same 15 baseline failures.

### 2026-11-17 — Type marker step 1

Form-agnostic Option/Result readers; member results from declarations. Same 15 baseline failures.

### 2026-11-17 — Type marker step 2

Annotations and native signatures produce Type::Enum; bridges for the remaining marker producers. Same 15 baseline failures; crate tests all pass.

### 2026-11-17 — Type marker step 2 follow-ups

Removed some/none/ok/err; Type::Enum is Arc; core enums stored inline. Same 15 baseline failures. Known: ~16% slower than the marker path on the Option/Result loop; match-per-iteration cell leak (see storagekind-removal.md).
