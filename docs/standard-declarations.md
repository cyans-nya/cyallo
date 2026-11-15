# Standard declarations and imports

The standard namespace is declared in [std.ya](../std.ya). Its ordinary `pub mod`
declarations load child files such as [std/task.ya](../std/task.ya). The same
resolver handles source and standard modules, visibility, grouped/glob imports,
aliases, reexports, shadowing, and associated declarations. The prelude is an
ordinary module of source reexports, identified by its private annotation.

```cyallo
#[cyallo_internal("task_struct")]
pub record Task<T> {}

#[cyallo_internal("task_spawn")]
pub proc spawn<T: Transferable>(body: proc() -> T) -> Task<T>;

impl<T> Task<T> {
    #[cyallo_internal("task_join")]
    pub proc join(self) -> TaskOutcome<T>;
}
```

The annotation binds a declaration to a private semantic ABI key. The compiler's
typed registry describes supported representations and operations; it contains
no public symbol paths. A native representation is opaque, and native callables
have bodyless declarations. An ordinary callable has a source body. Removing a
native annotation and supplying a body gives ordinary source behavior, even if
the public name remains the same.

The build bundles `.ya` assets by discovering files, without parsing exports or
generating a symbol catalog. An explicit core source provider grants provenance
to those bytes. Matching a standard filename or declaring a module named `std`
does not grant it. Untrusted internal annotations are rejected. The default
provider embeds the repository's standard package; host configuration may supply
another trusted source package through the same linker.

## Resolution and specialization

1. Parse declarations, annotations, generics, bounds, parameter modes, and members.
2. Assign compilation-owned declaration and module identities, retaining provider
   provenance and defining module across aliases and reexports.
3. Instantiate the actual source contract against resolved arguments and receiver
   types. Check effects, bounds, ownership, and module access.
4. Validate compatibility with the private native ABI, then emit typed IR.
   Execution engines consume resolved operations and types.

Unannotated associated functions with source bodies remain ordinary routines;
their publication identities are independent of declaration allocation order.
Only bodies containing native function values need the adapter pass's local
inference; other bodies retain deferred admission.

The immutable declaration environment is retained by deferred bodies, generic
specialization, ownership checking, and lowering. Native function items retain
declaration identity when bound to variables. Where an ordinary function type is
required, a checked, capture-free adapter instantiates the native contract.
Coercion preserves evaluation of the original expression, permits pure `fn` to
widen to `proc`, and rejects the opposite effect change. Value-dependent native
signatures require their compile-time values and cannot become arbitrary
callbacks. Private dependent type families and variadic/read contracts describe
native headers; they are not a general user type-level programming feature.

Core function identities are allocated before user declarations. Adding REPL
definitions therefore preserves saved native function items. IDs remain local to
their compilation; there is no global identity allocator.

## Scoped imports and file grants

An import policy is explicit compiler configuration. Its global ceiling and each
matching referrer scope intersect: a narrower scope cannot widen its parent's
authority. A scope targets either one declared module or its declared subtree.
Access sets name exact module targets, resolved once to actual module identities.
Allowing a parent does not implicitly allow its children. Misspelled configuration
paths produce diagnostics rather than silently changing access.

Permission checks use a symbol's defining module, including through aliases and
reexports. Native receiver calls retain that same check after type resolution.
Trusted core implementation imports may use internal dependencies; user modules
receive only their configured access. Passing a capability remains an explicit
language operation and does not expand the receiving module's native imports.

Filesystem loading separately follows declared `mod` edges within canonical read
grants. A child has exactly one source: `name.ya` or `name/mod.ya`. The loader
rejects ambiguous candidates, leaf symlinks, out-of-grant traversal, and canonical
cycles. It reads only declared files and retains source paths in typed failures.
Neither imports nor module discovery execute code. Module initialization permits
constants; effects belong in explicitly called procedures.

Every user/core file has its own source range in the compiled archive. Annotation,
parse, and runtime diagnostics preserve UTF-8 offsets and original filenames.

## Runtime source loading

[`std::module::load(path)`](dynamic-import.md) loads a new file into a readonly
dynamic namespace at runtime. Optional structural views retain that namespace.
Loading and compilation park the calling coroutine;
publication preserves its existing frames and scoped permissions. The standard
interface is another source declaration with a private ABI identity.

## Verification

[native_declarations.rs](../tests/native_declarations.rs) moves the task module and
renames its type, factory, and member using only `.ya` edits. It also checks source
fallback behavior, forged annotations, opaque storage, incompatible contracts,
core source diagnostics, import ceilings, scoped denial, and reexport origins.
[native_callbacks.rs](../tests/native_callbacks.rs) exercises aliases, generic
callbacks, effect checks, and exactly-once evaluation across eager/deferred
checking, all execution engines, GC stress, and instruction-budget handoffs.
[repl.rs](../tests/repl.rs) verifies identity persistence between submissions.

The [research report](intrinsic-declaration-research.md) records the read-only
Origine/V8 investigation and the rationale for these boundaries. Its descriptions
of the old catalog are historical evidence; this document describes the current
implementation.
