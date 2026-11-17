# Compiler and runtime rules

These requirements apply to the parser, resolver, type/effect/ownership checks,
IR, optimizers, execution engines, runtime crates, and their examples. They record
the project's implementation constraints, rather than a claim that every existing
path has already been migrated.

## Typed status and error models

Represent each status, outcome, kind, and failure with an enum whose variants
state the actual alternatives. Preserve that enum through internal calls and
matches. Do not introduce magic `u8`/integer tags for semantic states or kinds.
Native or serialized representations may use explicit enum discriminants at the
encoding boundary; validate conversion and keep internal operations typed.

Use concrete error enums implementing `Display` and `std::error::Error`. Return
`Result<T, ErrorType>`, retaining the error variant, its source, and relevant data.
Never return error strings (`String` or `&str`) or classify failures by matching
their formatted text. Format errors when producing a diagnostic or presenting
them to a user. Error variants may retain structured causes, source spans, names,
and user-provided panic payloads; a free-form message is not an error category.

Keep task completion, runtime failure, cancellation, and abrupt stop distinct.
A returned `Result::Err` is an ordinary value, not a runtime failure. Likewise,
channel closed/full/quota failures and generator startup/resume/close failures
need explicit variants. Resource recovery belongs in the typed error payload;
for example, failed sends retain their unsent value.

Convert typed failures to diagnostics through descriptive helpers such as
`invalid_numeric_literal` and `yield_outside_generator`. The helper owns the
diagnostic-code mapping and presentation. Call sites must not pass hardcoded
error-code literals to a generic conversion function.

When modifying a legacy string-error path, migrate its error handling to the
typed model. A generic string-carrying fallback variant does not complete that
migration. Test error variants and their payloads; test `Display` separately when
the rendered message is part of a user-facing contract.

## Builtins follow the language pipeline

Use this pipeline for builtin primitives as well as user-defined operations:

```text
tokens → source AST → declaration/name resolution
→ type, generic, effect, ownership, and capability resolution
→ typed IR with resolved operation identities
→ type-directed specialization → bytecode/native execution
```

Parse language syntax into AST nodes and operators. Resolve ordinary calls and
members to their declarations, including aliases, imports, shadowing, overloads,
and user-defined members. Standard declarations may carry stable intrinsic or
builtin-type identities. Instantiate their contracts using the resolved operand,
receiver, and type-argument information before lowering.

Dereference is the postfix operator `value.*`, composable as `value.*.*`,
`value.*.field`, or `value.*[i]`. Keep multiplication as `a * b`. Reject prefix
`*value` with its migration diagnostic. Dereference and multiplication retain
distinct typed operation identities; execution never recognizes source text.

Never hardcode builtin primitive behavior by string-matching source names in
lowering, optimization, code generation, or execution. A name such as `spawn`,
`share`, `Cell`, or `resume` does not, by itself, identify an operation. Source
strings belong in lexical/declaration lookup, display, and diagnostics. A
public namespace comes from real `.ya` declarations and ordinary `mod`/`use`.
Do not keep a parallel Rust list of standard paths, exports, member names,
prelude names, or signatures. Trusted declarations bind private semantic ABI
keys through `#[cyallo_internal("...")]`; the compiler validates those bindings
and specializes their instantiated contracts. Renaming or moving a standard
declaration requires only source edits. See
[standard declarations and imports](standard-declarations.md) for the boundaries
and verification requirements.

For example, an alias of a standard primitive retains that primitive's identity.
A user declaration with the same spelling retains the user's behavior. A user
type's `resume` member must resolve to its own declaration. An ordinary sharing
function is checked using its uniqueness and sharing types, never recognized by
the spelling `share`.

Carry semantic enums, declaration IDs, instantiated signatures, effects, layouts,
and ownership/capability information into IR. Backends consume that information;
they do not re-parse source text or reconstruct semantics from diagnostic names.
If a method primitive needs a new identity, define and resolve that identity
instead of adding a name comparison in a later compiler stage.

This follows the requested V8-inspired separation of interpreter representations
and optimizing compilation. V8 documents its register-based
[Ignition interpreter](https://v8.dev/docs/ignition) and
[TurboFan optimizing compiler](https://v8.dev/docs/turbofan); the requirements in
this document define Cyallo's own typed pipeline.

## Specialization uses resolved information

Select specialized operations from proven types, layouts, declaration identity,
and effect/ownership information. Preserve those facts through generic
instantiation, closure conversion, trait dispatch, and deferred analysis.

Use guarded specialization when a dynamic target or layout is only observed.
Check its assumptions before committing effects and retain the checked generic
operation as fallback. Preserve source PCs, exception/failure behavior, fuel
accounting, GC roots, and capabilities. Inlining or specialization must not repeat
side effects, promote local data to shared, or bypass a failed check.

Verify aliases and shadowing, user methods with matching spellings, instantiated
generic types, and fallback when specialization assumptions do not hold.

## Runtime state has explicit owners

Own registries, identity allocation, channel coordination, quotas, timers,
blocking pools, code publication, and cancellation state in a Runtime, Session,
factory, or explicitly passed capability. Independent instances must work
independently, including when their scoped numeric IDs happen to be equal.

Never create mutable process-wide counters, registries, or singleton services
such as `NEXT`, `NEXT_RUNTIME`, or `SERVICE`. The one sanctioned process-wide
resource is the compile worker pool in `cyallo-compile-jobs`: it is capacity only (idle
threads keyed by `Jobs` width, no identifiers, registry or program state), compile entry
points have no owner to hold it, and runtimes and sessions share nothing through it. Do not
extend that exception to anything that holds state. Do not use thread-local executor
state to discover the current runtime or processor. Carry coroutine identity,
processor preference, fuel, and wake capabilities explicitly.

Keep GC/RC storage choices separate from local/shared/unique qualification.
Spell storage as `gc`/`rc`, with optional `@local` or `@shared` before it. Local
is the default. Preserve locality in AST and resolved types, including through
generic substitution and type comparison. Reject the old `@gc`/`@rc` syntax.
Until shared graph support exists, reject `@shared` with a typed diagnostic;
never erase it or execute it with local semantics. The current syntax and limits
are documented in [managed memory](managed-memory.md).
Default-local managed graphs stay with their coroutine. Sharing requires the
appropriate checked conversion; an accepting function cannot silently promote a
local value and send it to another coroutine. Validate captures, saved state,
results, and erased values as well as direct arguments.

Each executing T needs a P from the same budget. Pending operations release it.
Register an operation, its result destination, and its successor PC before the
host effect begins. Retain owned buffers or rooted descriptors across Pending;
never retain borrowed guest pointers. Generator yield returns to its resumer;
waiting for I/O parks the whole C. Saved VM/native roots and code snapshots must
remain valid across both transitions and collection.

## Completion is verified in Cyallo

A language feature is complete only when a Cyallo source program parses,
resolves, type-checks, lowers, and executes through the VM with its documented
behavior. Runtime-only Rust scaffolding or Rust examples do not establish that.

Put language examples in `.ya` files with Cyallo CLI commands and expected
output. Rust is for runtime/compiler implementation and verification tests.
Do not add Rust example programs or external integration work unless explicitly
requested by the user. Label pending features accurately and update status only
after the corresponding checks pass.

For changes affecting suspension, ownership, or GC, exercise eager and deferred
checking, interpreter and native engines, GC stress, and instruction-budget
handoffs. Include rejected Cyallo programs for invalid types, borrowing,
uniqueness, sharing, capture, and capability use relevant to the change. Verify
language semantics at their public interfaces and keep responsibilities in
focused crates/modules.
