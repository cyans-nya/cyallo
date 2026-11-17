# Data enum implementation

Current handoff: [pending work](pending-work.md). The unfinished task patch
currently fails compilation. The behavior documented below was verified before
that patch; it does not establish completion of the current worktree. The user
has rejected the StorageKind-based std type model, so its removal is required
even for the previously exercised Option/Result/Step/channel declarations.

The enum/`Option`/`Result` milestone executes on branch `data-enums`. The remaining
standard status/error migrations are listed below; the whole draft is not yet
complete. The [work log](data-enums-work-log.md) records decisions, failed checks,
fixes, verification and the resume point. The target is the
[data enum draft](data-enums-draft.md).

## Executable

- Nominal enum and variant declaration identities survive imports, aliases,
  reexports, generic instantiation and checked-module boundaries. Constructors
  are callable declarations; unit variants are values. Unused generic enum
  templates are checked for invalid value recursion and type-argument arity.
- Generic payload inference and expected constructor types, including unit
  variants, returned values, managed construction and constructor function values.
  Generic instances remain in their defining modules.
- Structural anonymous record types/literals, canonical field layout, source-order
  field evaluation, field access/mutation, cloning and equality. Naming a
  structural type does not give it nominal identity. Structural records never
  implicitly coerce to declared records.
- Variant brace construction supplies an anonymous or declared record payload;
  generic declared-record fields participate in inference.
- Nested variant, tuple, record, literal, wildcard and binding patterns; record
  rest patterns, alternatives and guards. Alternatives agree on binding names and
  types. Pattern-matrix coverage accounts for nested payloads. Guards do not count
  toward coverage. Empty enums and diverging arms retain an internal bottom type.
- Match expressions/statements and `if let`, arm-local scopes, block tails and
  returns from the enclosing routine. Foreign open enums require an unguarded
  wildcard; same-package matches use ordinary exhaustive checking. `#[since]`
  remains declaration metadata and does not replace private ABI annotations.
- Value matches move non-copy bindings after a guard succeeds. Shared/mutable
  matches project real payload cells. Guards alias existing storage and cannot
  move or mutate matched storage. Partial payload moves release the remaining
  owned parts. Branch states retain moves and borrow provenance; borrowed payload
  and closure escapes are rejected.
- Typed construction, tests and projections through bytecode and all execution
  engines. Snapshots, clone/drop, move clearing, moving GC, constants, trait
  dispatch, closure captures and dynamic round-trips retain enum identity.
- Heap-independent enum payload transport through tasks/channels. Local managed
  values and borrows remain rejected, including inside structural payloads. See
  [the concurrency example](../examples/concurrency/enums.ya).

## Standard Option and Result

`std/option.ya` and `std/result.ya` now declare actual generic enums. Their
Some/None/Ok/Err declarations supply variant IDs and payload contracts to the
common constructor, pattern, coverage and ownership machinery. Existing
convenience methods and `some`/`none`/`ok`/`err` APIs remain; those constructors lower
to the same typed enum construction operation.

Private, validated annotations select optimized Option/Result ABI encodings.
`Type::Storage` remains their internal ABI marker; it does not supply public
variants or signatures. The compiler retains and instantiates the source enum
contract. Borrowed projections materialize rooted payload storage while snapshots
preserve the native ABI. Runtime behavior does not recognize public names.
Renaming standard variants in trusted source preserves native producer behavior.
An opaque record cannot claim the Option/Result enum ABI.

## Verification

[`tests/data_enums.rs`](../tests/data_enums.rs) covers eager/deferred checking,
interpreter, Winch, Cranelift and Cranelift speed, normal/stress GC, continuous
execution and alternating single-instruction handoffs. It compares output, status
and instruction counts. It also checks invalid patterns, types, moves and borrows.

[`tests/native_declarations.rs`](../tests/native_declarations.rs) covers standard
variant renaming, malformed native enum contracts, version annotations and foreign
open enums. Resolver/specializer checks are in
[`enum_resolution.rs`](../crates/cyallo-ir/tests/enum_resolution.rs); parser and
span/visitor coverage remains in the AST/parser suites. Concurrency tests check
actual cross-session enum transport. CLI tests run the
[documented examples](../examples/enums/README.md) in all four engine modes.

Full check results and the exact final checkpoint are recorded in the work log.

## Remaining draft work

Replace the rejected standard-library type catalogue with ordinary source types,
traits and a general native binding. Option/Result, generator Step and channel
errors have source enum behavior but still require this representation correction.
JoinResult/TaskFailure is unfinished; ViewError and LoadError still need their
source enum migration. Preserve typed causes and owned resource payloads. See the
ordered handoff for the full scope and verification required.

`?`, `let ... else`, anonymous enums and explicit discriminants remain design
questions in the draft. The component/world and types-as-values drafts are
separate work.
