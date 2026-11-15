# Declaration-owned standard primitives

Research date: 2026-11-14. This is a design report, not a claim that the proposed
syntax or migration is implemented. Origine and its V8 checkout were inspected
read-only. Cyallo was under active development during inspection; local line
links describe the inspected working tree. Recorded HEADs were Cyallo
`5bb3ccb17842406f69e1e5d30bbe3cdc4733c1ec`, Origine
`c2961cc339aabdde24d964248ef8fb19ee9f2dcd`, and its local V8 15.8.30 checkout
`a98309795399032afd2f191adaa94ed6e9918437`.

The resulting implementation is documented in
[standard declarations and imports](standard-declarations.md). References below
to the removed Rust catalog describe the inspected baseline.
The current managed syntax uses `gc`/`rc` with separate locality modifiers; any
`@gc`/`@rc` spellings below are historical baseline syntax.
Prefix dereferences below are also historical; the current syntax is `value.*`.

## Recommendation

Make real Cyallo declarations the authority for the standard namespace. A
standard package contains `std.ya`, whose ordinary `pub mod task;` declaration
loads `std/task.ya`. That file declares `Task`, its generic parameter and members,
and the callable contracts. A trusted declaration annotation such as
`#[cyallo_internal("task_struct")]` binds the actual declaration to a compiler
semantic identity. Imports, aliases, reexports, visibility, overload selection,
and shadowing use the same declaration resolver as user code. After resolution,
checking and lowering consume declaration IDs and instantiated contracts.

Keep a small compiler-owned registry of **private semantic ABI keys**, such as
`task_struct`, `task_spawn`, and `task_join`, with typed semantic enums and ABI
validation rules. Remove registries of public paths, modules, exports, types,
prelude names, and member spellings. Moving a declaration or renaming its public
spelling must require no compiler change. Adding a genuinely new runtime
operation can require one. This recommendation implements the requested
declaration-based architecture; it does not require a second language, Rust
example programs, or an alternate public module system.

This distinction follows Cyallo's required declaration → type/effect/ownership/
capability checking → typed IR → specialization pipeline. The current rules
permit a declaration catalog, but the user's stronger requirement makes the
catalog actual `.ya` declarations rather than a parallel Rust public-name table.
[Project pipeline requirements](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:39)

## What current Cyallo already supplies

| Existing seam | Evidence and implication |
| --- | --- |
| Ordinary module syntax | The parser supports `mod`, inline module bodies, `use`, aliases, grouped imports, globs, and public reexports. Reuse these constructs for the standard package. [Parser](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/modules.rs:21) |
| Native file module loading | The host loads a declared child from `name.ya` or `name/mod.ya`, rejects ambiguous sources and leaf symlinks, tracks active canonical files, and parses modern Cyallo. Extend this loader with a package source provider instead of introducing a standard-symbol resolver. [Loader](/Volumes/Dev/x/cyallo/src/modules.rs:241) |
| Real declaration/import resolution | `Resolver::add` builds nodes for source modules; ordinary symbols carry owner and visibility. Import installation reaches a fixed point. These are useful foundations for loading a second package root. [Module nodes](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:67), [imports](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:324) |
| Existing compile-owned immutable metadata | Analysis builds an `Arc<services::Metadata>` and deferred bodies retain it through `DeferredContext`. Use the same ownership pattern for resolved declaration/member metadata. [Analysis setup](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:91), [deferred context](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:388) |

The incompatible current behavior is concrete: `stdlib.rs` manually enumerates
`PRELUDE`, `MODULES`, `TRAITS`, `TYPES`, and `FUNCTIONS`. `standard(path)` fabricates
standard symbols from that table; path traversal and glob imports take a separate
standard-only branch; type resolution returns to the public-path table to choose
storage semantics. Replacing only the `spawn` entry would leave this architecture
intact. [Catalog](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/stdlib.rs:22),
[fabricated symbols](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:48),
[special path traversal](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:247),
[type conversion](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:449)

Members also need migration. `concurrent::method` currently matches
`(StorageKind::Task, "join", ...)`, while `lookup` has similar member signature
tables for other primitives. Inference, ownership checking, and lowering each
call these name-based helpers. Ordinary source-name lookup is appropriate in a
member resolver; assigning an operation directly from a spelling in these
downstream helpers is the missing declaration boundary.
[Concurrent members](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/concurrent.rs:109),
[other members](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/lookup.rs:433),
[inference](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/inference.rs:350),
[ownership](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/ownership.rs:561),
[lowering](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:1220)

The synthetic allocator trait is another declaration source outside `.ya`:
`namespace::finish` injects `memory::allocator_declaration`, which constructs the
public trait name and methods in Rust. Migrate this into trusted source too;
keep only its semantic trait identity and structural ABI requirements in Rust.
[Injection](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/namespace.rs:955),
[synthetic trait](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/memory.rs:73)

## What Origine demonstrates

Origine has two related, distinct kinds of scope. A managed `Scope` owns a V8
context and module map; its options include resolver, loader, an independently
copied import map, namespace permission, and per-module metadata injection.
Its import-map scopes are **referrer-prefix mappings**, not lexical block import
syntax. Mappings are sorted by specificity, consult scoped entries before global
entries, allow blocked targets, and reject prefix backtracking. Cyallo can reuse
these authority and referrer-context principles without copying JavaScript URL
syntax into its native lexical `use` system.
[Scope options](/Users/cyan/Documents/key/nya/origine/src/managed/scope.zig:141),
[scope ordering](/Users/cyan/Documents/key/nya/origine/src/managed/import_map.zig:33),
[blocked/prefix mapping](/Users/cyan/Documents/key/nya/origine/src/managed/import_map.zig:188),
[scoped resolution](/Users/cyan/Documents/key/nya/origine/src/managed/import_map.zig:239)

Its JavaScript standard library is real source using ordinary imports. For
example, `fs.js` imports quansync, I/O, and text declarations. Graph source
descriptors are inert data and carry the explicit filesystem grant used to read
a file. Merely constructing or importing a descriptor does not grant authority.
[Standard JS imports](/Users/cyan/Documents/key/nya/origine/src/js/std/fs.js:18),
[module descriptors](/Users/cyan/Documents/key/nya/origine/src/js/std/graph/module.js:1)

The useful security mechanism is **trusted source provenance**, not the reserved
spelling alone. The init loader serves embedded modules from host-owned source,
refuses to load working-directory files into the reserved namespace, and injects
natives only into the matching host module. A child context records which IDs
were actually served natively; access to its private handles module requires
both a trusted host module and recorded native service. A user resolver returning
a convincing ID cannot manufacture that provenance.
[Source/native injection](/Users/cyan/Documents/key/nya/origine/src/api/init_loader.zig:52),
[private trust check](/Users/cyan/Documents/key/nya/origine/src/api/internal.zig:13),
[served-source check](/Users/cyan/Documents/key/nya/origine/src/api/context.zig:518)

Capability delegation is bounded. A child import handler cannot expose more of
the standard namespace than its creating realm's ceiling. File-source grants
must belong to the realm and permit reading. Runtime filesystem operations check
read/write authority, and grant lookup checks the issuing owner and liveness.
For Cyallo, visibility/import permission and runtime capability possession should
therefore remain separate checks; aliasing a function or spawning a coroutine
must preserve both rather than widening authority.
[Delegation ceiling](/Users/cyan/Documents/key/nya/origine/src/api/graph.zig:861),
[source grant](/Users/cyan/Documents/key/nya/origine/src/api/graph.zig:909),
[grant rights](/Users/cyan/Documents/key/nya/origine/src/api/fs.zig:230),
[grant ownership](/Users/cyan/Documents/key/nya/origine/src/api/fs.zig:468)

Origine still has an explicit host table of embedded module descriptors. That is
not evidence for retaining Cyallo's forbidden public-symbol inventory. Borrow
the separation of source, provenance, policy, and authority; use Cyallo module
declarations to discover its standard tree.
[Origine embedding table](/Users/cyan/Documents/key/nya/origine/src/api/std/modules.zig:1)

## What the actual V8 source demonstrates

Three V8 mechanisms are easy to conflate:

1. **Runtime intrinsic ABI.** `runtime.h` defines runtime versus inline
   intrinsic descriptors and generates `Runtime::FunctionId`. Descriptors include
   argument count, result count, and fallback entry point. The runtime intrinsic
   parser resolves an internal name to that descriptor, checks arity, and builds
   a `CallRuntime` AST. Bytecode generation reads `expr->function()->function_id`;
   optimization dispatches on that ID. A backend can decline a specialization
   and retain the runtime call. These are private engine operation names, not a
   registry of public JavaScript paths.
   [Registry and fallback contract](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/runtime/runtime.h:20),
   [typed descriptors](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/runtime/runtime.h:959),
   [parser resolution](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/parsing/parser.cc:553),
   [bytecode](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/interpreter/bytecode-generator.cc:7403),
   [intrinsic lowering](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/compiler/js-intrinsic-lowering.cc:28)
2. **Typed Torque declarations.** Real `.tq` files declare external builtins,
   runtimes, and generic compiler intrinsics with parameter and result types.
   The declaration visitor constructs signatures and validates runtime ABI
   constraints; generic specialization produces a specialized signature.
   Calls to transitioning operations require a transitioning caller. Generated
   runtime calls refer to the resolved callable's external ABI identity.
   [Example declarations](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/builtins/base.tq:919),
   [generic intrinsic](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/builtins/base.tq:160),
   [declaration validation](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/torque/declaration-visitor.cc:162),
   [specialization](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/torque/declaration-visitor.cc:425),
   [effect check](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/torque/implementation-visitor.cc:3015),
   [generated runtime call](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/torque/csa-generator.cc:713)
3. **Public callable specialization.** `JSCallReducer` obtains a builtin ID
   from the actual function's shared metadata. For array push it additionally
   needs receiver map information, supported element layouts, protector
   dependencies, and map stability/checks. It does not specialize because the
   source happened to spell `Array.prototype.push`. This is the closer analogy
   for resolving a Cyallo imported callable or member and then specializing it.
   [Function identity](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/compiler/js-call-reducer.cc:5407),
   [receiver guards](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/compiler/js-call-reducer.cc:6569)

The official Torque manual confirms the distinctions between external builtins,
runtime callbacks, and compiler intrinsics, and documents generic signatures and
transitioning effects. These support the architecture; they do not make Torque
an implementation requirement for Cyallo. [V8 Torque manual](https://v8.dev/docs/torque)

Origine's V8 patch adds a fourth, host-only facility: retaining original public
builtin function objects automatically as the engine installs them. The table
is context-owned, indexed by builtin ID, and fails closed when one ID denotes
distinct installed functions. Its documentation explicitly distinguishes builtin
names from numeric IDs and warns against exposing unrestricted lookup to guest
code. This reinforces deriving identity from actual installed declarations or
objects rather than reconstructing an inventory by traversing public names.
[Actual patch](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/build-tools/patches/v8/0001-intrinsics.patch:9),
[host API contract](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/docs/intrinsics.md:121)

“Stable semantic ID” here means an identity controlled at the compiler/runtime
boundary. V8's generated enum ordering is not evidence of stable numeric values
across engine versions. Cyallo should version serialized bytecode/cache ABI or
explicitly assign stable discriminants if persistence requires them.
[Generated V8 IDs](/Users/cyan/Documents/key/nya/origine/lib/v8_zig/.lp-cache/v8-15.8.30/src/runtime/runtime.h:961)

## Concrete migration shape

### Source loading and provenance

Introduce a compilation-owned package/module graph and source provider. The
compiler configuration supplies the standard package root once, loads `std.ya`,
and follows its ordinary module declarations. The language package name `std`
can be bound to that root in the package environment; this is one package-root
binding, not a per-symbol semantic registry. Use actual `std/prelude.ya` exports
for implicit prelude imports. Query all exports from module declarations.

For distribution, source may be read from the installed standard directory or
embedded as an automatically packaged source archive. An automatically generated
file index is packaging metadata only: it must contain no `BuiltinType`,
intrinsic operation, public export, or prelude mapping. The same module loader
and parser process disk and bundled sources. Do not hand-maintain one
`include_str!` entry per standard module as a substitute for module discovery.

Each loaded source gets unforgeable loader-issued provenance, conceptually
`User` or `TrustedCore(package identity)`. General `SourceModule::new(path,
source)` remains untrusted; its caller-controlled path cannot confer trust.
An internal provider attaches provenance to module/source IDs and carries it
through source concatenation, parsed units, span shifting, inline children, and
the linker. Loading a dependency from another provider does not inherit trust
merely because its parent was trusted. Existing archives and deferred compilation
must retain this information. [Current source constructor](/Volumes/Dev/x/cyallo/src/modules.rs:56),
[concatenation/indexing](/Volumes/Dev/x/cyallo/src/modules.rs:165)

An explicit developer configuration may choose another trusted core root, but
that is a host trust decision. Neither a directory named `std`, a `std::` source
path, a source attribute, nor a guest import alias is a credential.

### Parsed declarations and the semantic table

Keep `RoutineAttributes` focused on existing compilation hints. Add declaration
annotations, with spans and literal arguments, to the declaration AST. Parse
annotations before dispatching to module/type/trait/routine parsers. Preserve
raw keys until semantic validation. The current parser accepts only identifier
values for routine attributes, rejects them on records/modules, and treats
trait declaration bodies as empty vectors; it therefore needs a deliberate AST
extension rather than another hint field.
[Current attributes](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/modern.rs:13),
[declaration dispatch](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/lib.rs:168),
[routine body construction](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/modern.rs:311)

Recommended metadata responsibilities:

| Record | Required information |
| --- | --- |
| Parsed declaration | Source name/span, declaration annotations, generic binders, types, visibility, and a distinct source-body/native-body kind. An empty source body is not a native binding. |
| `DeclarationId` | Compilation-owned nominal identity; aliases/reexports refer to the same ID. Public strings remain lookup keys and diagnostic names. |
| Declaration entry | Owner `ModuleId`, provenance, kind, visibility, nominal type identity or callable signature, validated optional internal semantic identity. |
| Callable contract | Generic binders and constraints, receiver mode, parameter ownership/regions, result type, effects, suspension/failure properties, and required capabilities. |
| Member index | Actual member declarations indexed by owner nominal declaration and source name; lexical visibility/import candidates and ambiguity are resolved here. |
| Instantiated callable | Origin declaration ID, owner and method substitutions, concrete signature, ownership/effects/capabilities, and optional runtime operation ID. |
| Typed IR call | Resolved callable/operation identity, instantiated types and layouts, operands, result destination, effects, and continuation metadata. |

The marker on the **actual Task declaration** supplies native representation
identity. Its methods are real declarations in an ordinary inherent
implementation. The following illustrates the proposed extension; it is not an
executable example for today's parser:

```cyallo
// std.ya
pub mod task;
pub mod prelude;

// std/task.ya
#[cyallo_internal("task_struct")]
pub record Task<T> {}

impl<T> Task<T> {
    #[cyallo_internal("task_join")]
    pub proc join(self) -> TaskOutcome<T>;
}
```

An annotated native empty record must be opaque to source construction; the
empty field list is not permission to fabricate a task handle. Its layout/drop/
trace behavior comes from the validated semantic representation identity.
`TaskOutcome` and other native declarations need their own real definitions and
contracts. The syntax for opaque records, bounds, effects, and native bodyless
declarations should be explicit and validated; do not silently treat every
ordinary empty record or bodyless callable as native.

### Validate the private ABI once, then resolve normally

Validation should produce concrete typed errors for unknown keys, use in
untrusted sources, unsupported declaration kinds, duplicate canonical type
bindings, conflicting markers, wrong generic arity, wrong receiver mode,
incompatible parameter/result shape, and missing ownership/effect/capability
requirements. Preserve both source spans for conflicting declarations. Existing
project rules require typed internal errors and diagnostic helpers rather than
classifying error strings. [Error requirements](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:10)

The ABI registry may say that `task_struct` identifies a one-parameter native
type, or that `task_join` consumes that type and produces the corresponding
outcome while possibly suspending. It must not say that this type lives at
`std::task::Task`, that the source method is named `join`, or which public module
exports it. Reexports do not count as new ABI declarations. If several internal
declarations deliberately share an operation implementation, define and validate
that multiplicity rather than overwriting a singleton entry.

Types and signatures come from the parsed declarations, instantiated by normal
checking. ABI validation rejects incompatible declarations; lowering must not
silently replace their signatures with a second handwritten public contract.
Keep irreducible runtime constraints in semantic validators: for example,
transportability must include captures, results, nested graphs, and erased
values. Generic/effect/ownership requirements that source syntax cannot yet
express remain an explicitly tracked language gap; passing an annotation alone
does not complete the requested typed declaration design.
[Concurrency checking today](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/concurrent.rs:65),
[transport requirements](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:91)

### Share member resolution and preserve generic origin

Create one immutable declaration environment owned by the compilation. Inference,
ownership checking, eager lowering, and deferred lowering consult it. A member
lookup first resolves the actual receiver nominal type plus lexical scope to a
member declaration, then substitutes owner/method generic arguments, checks the
contract, and finally selects the internal operation. Plain user methods named
`join`, `send`, or `resume` retain their own behavior.

Reuse the existing ordinary method selection and ambiguity structure in
`services::Metadata`, but do not pretend it already handles inherent generic
native methods: it currently stores trait implementations with exact receiver
types, and the parser requires `impl Trait for Type`. Add a distinct inherent
implementation representation with generic binders and explicit owner identity.
[Method selection](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/services.rs:56),
[current impl grammar](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/modules.rs:135)

Preserve `(origin DeclarationId, type arguments)` when materializing a native
type's representation. Current record monomorphization clones fields and renames
the record; routing native `Task<T>` through that unchanged would risk producing
an ordinary empty record and losing its semantic owner. Likewise, its initial
“no generics” shortcut examines only records and top-level functions; new generic
implementation templates must participate in specialization. Cache instances by
declaration identity plus canonical substitutions, not public path or formatted
diagnostic name. [Record instances](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/generics.rs:120),
[function instances](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/generics.rs:145),
[specialization entry](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/generics.rs:369)

Retain the frozen environment in `DeferredContext` alongside the existing
services metadata. This prevents lazy analysis from reconstructing a different
catalog or discovering mutable process-global state. Native bodies do not
produce ordinary source routine bodies, but callable references must still
support import aliases, local aliases, and first-class invocation without
depending on the direct-call spelling. [Deferred ownership seam](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:388)

### Permissions and runtime execution

Keep lexical imports in the owning module or block environment; do not flatten
them into universal extension availability. Import policy applies to the
requesting module and the resolved module identity, before installing bindings,
so aliases and reexports cannot bypass it. The standard prelude is an ordinary
implicit import from a resolved prelude module. If block-scoped `use` is desired,
it needs AST scope representation; the current `Ast.imports` list is module-level.
[Current import AST](/Volumes/Dev/x/cyallo/crates/cyallo-ast/src/lib.rs:340)

Required runtime authority must be carried by explicit capability values or a
sealed, explicitly passed execution context admitted by the compiler/host. Do
not mint a runtime, spawner, filesystem grant, or clock because a standard module
was imported. Child coroutines inherit only the admitted capabilities they are
given, with the same owner checks. Introducing new public capability parameters
would be a separate API decision; the declaration migration can preserve public
calls while making existing VM execution-context authority explicit internally.
This matches Cyallo's existing instance-owner and suspension contracts.
[Runtime ownership rules](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:84),
[concurrency capability contract](/Volumes/Dev/x/cyallo/docs/concurrency.md:127)

The runtime receives typed operation enums, layouts/transport information, and
instance-owned context. Suspension operations retain destination and successor
PC before starting effects. Specialization uses proven types or checked guards
with a generic operation fallback, preserving effects, failures, roots, and
fuel. No runtime or backend needs to know the declaration's public path.
[Specialization rules](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:70),
[single-commit operation contract](/Volumes/Dev/x/cyallo/docs/concurrency.md:70)

## Migration and verification sequence

1. Add declaration annotations, native/source body distinction, inherent generic
   implementations, and source provenance. Keep this metadata compilation-owned.
2. Load actual `std.ya` and its declared children through the package/module
   loader. Move all standard types, traits, routines, members, and prelude exports
   into source. Delete the public path inventories and synthetic allocator trait.
3. Bind validated private ABI keys to declaration IDs; replace standard-only
   namespace/type/export branches with ordinary resolution.
4. Introduce the shared generic member resolver and migrate inference,
   ownership, effects, and lowering to instantiated contracts. Preserve native
   nominal origin through generic specialization and deferred bodies.
5. Keep runtime operations typed and instance-owned. Existing runtime behavior
   need not change solely because its declaration moved into `.ya`.
6. Verify with Cyallo programs, including import aliases/reexports, local and
   first-class aliases, shadowing, user members with matching names, generic
   native methods, rejected forged annotations and malformed signatures, private
   visibility, scoped permission ceilings, and independent runtime instances.

The decisive architecture test is to rename or move a trusted standard
declaration in a temporary source fixture, update only source imports, and
observe unchanged operation behavior without changing Rust. Also verify that
removing its annotation removes special semantics, and that copying the same
annotation into untrusted source is rejected. Exercise relevant concurrency
examples under eager/deferred checking, interpreter/native engines, moving GC
stress, and instruction-budget handoffs as required by the project.
[Completion criteria](/Volumes/Dev/x/cyallo/docs/compiler-and-runtime-rules.md:117)

Review points before treating the migration as complete are the exact opaque
native declaration syntax, how generic bounds/effects/ownership are expressed in
source, whether block-level imports are in scope, and whether any new public
capability arguments are intended. These are language/API choices, not reasons
to retain a public-path registry. No proposal here depends on editing Origine
or its V8 clone.

## Appendix: smallest correct callable and permission migration

Follow-up investigation distinguishes a narrow plumbing change from the language
contracts it must preserve. The existing `Type::Function` carries only routine
kind, a fixed parameter vector, and result type. `analysis::Signature` adds an
ordinary routine index. Neither can represent a heterogeneous variadic native
call, generic bounds, capability requirements, or a declaration-owned native
target. Do not force such a declaration into `proc()` or `proc(Any)` and then
ignore that signature in an intrinsic-specific call checker.
[Callable type](/Volumes/Dev/x/cyallo/crates/cyallo-ast/src/lib.rs:104),
[lowering signature](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:11)

### One environment, one call-instantiation result

The smallest useful plumbing change is a `TypeEnvironment` containing the
existing routine/record type map plus an `Arc` to immutable declaration/member
metadata. Keep the local `known` map unchanged. Change the parameter type from
`&HashMap<String, Type>` to `&TypeEnvironment` in inference, ownership,
contextual argument inference, composite helpers, generic instantiation, and
lowering. A temporary `Deref<Target = HashMap<...>>` can preserve routine map
operations, but it is not the metadata mechanism: retaining an old `&HashMap`
function signature silently drops access to the declaration environment. Prefer
explicit `.types` access in new code and expose actual semantic lookup methods
on the wrapper. Do not implement `DerefMut` on a frozen published environment.
[Inference entry](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/inference.rs:21),
[ownership state](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/ownership.rs:71),
[lowering state](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:28)

Freeze this environment after declaration collection and instance preparation;
store an `Arc` in deferred context. A scope view supplies the current `ScopeId`
separately or as a cheap borrowed view, so lexical lookup is not confused with
the global map. Resolve a call/member once into an `InstantiatedCall` containing
the declaration ID, substitutions, expanded parameter list, receiver mode,
result, effects, required capabilities, and typed native/ordinary target. Reuse
this result across checking and lowering, or have pure environment methods
produce the same result before side effects. Contextualize against the expanded
parameter types; ownership uses the same parameter modes. An `IntrinsicId` alone
in an AST call loses the declaration contract and is insufficient as the final
migrated representation.
[Current intrinsic call path](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:2062),
[deferred retention](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:388)

For a concrete first-class native function reference, the smallest runtime
adaptation is a compiler-generated ordinary thunk with the instantiated checked
signature and a typed native operation in its body. For a local alias of an
uninstantiated generic or variadic declaration, retain a declaration reference
and substitutions until invocation; do not erase it to a fixed `Type::Function`
before specialization. General runtime higher-rank polymorphism is a separate
feature and is not needed just to preserve statically known aliases.
[Current indirect call ABI](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:1384)

### Numeric generics and heterogeneous print

Modern min/max currently require two values of the same numeric type and return
that type; clamp requires three. Their literal contextualization is currently
selected by intrinsic identity. Ordinary generic inference currently first
computes all argument types, then unifies them, so simply moving these routines
to unconstrained source generics would change literal inference and admit
nonnumeric types. Move contextual inference into generic signature instantiation:
bind shared type variables from explicit types and nonliteral operands first,
then contextualize unsuffixed literals. When no stronger constraint exists, use
the existing numeric literal defaulting policy. A mismatching typed value is a
type error rather than an implicit numeric conversion.
[Numeric contract](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/builtins.rs:64),
[current contextualization](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:741),
[generic inference](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/generics.rs:333)

Recommended minimal source contract forms, proposed rather than currently
implemented syntax, are:

```cyallo
#[cyallo_internal("numeric_min")]
pub fn min<T: Numeric>(left: T, right: T) -> T;

#[cyallo_internal("print")]
pub proc print<...Ts: Printable>(...values: Ts);
```

`Numeric` and `Printable` must resolve to actual declared traits; compiler-known
primitive trait markers may supply semantic membership predicates without
recognizing their public names. For current print compatibility, `Printable`
means precisely numeric primitives, string, char, and bool, not every value or
an unchecked `Any`. A pack is zero or more independent type arguments, each
satisfying its bound; expanding it preserves heterogeneous calls such as
`print("answer=", 42, true)` and zero-argument printing. It is a compile-time
tuple of parameter types, not a C variadic ABI or mandatory heap array.
An ordinary homogeneous `...values: T` cannot express today's print contract.
[Current variadic scalar contract](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/intrinsic.rs:243),
[current output lowering](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/analysis.rs:2098)

If implementing full generic packs immediately is disproportionate, an explicit
source rest parameter with an independently checked trait constraint per
element is the narrow alternative, with precisely those semantics documented.
It must still appear in parsed parameter metadata and be expanded by the shared
signature checker. An annotation claiming a fixed source signature is variadic
only in lowering is not sufficient.

Existing `fn` versus `proc`, reference types, and generic parameter names can
express some purity/ownership contracts. The inspected parser lacks generic
bounds, type packs/rest parameters, named effect sets, and source capability
requirements. Use explicit parsed contract syntax or structured contract
annotations that all semantic passes validate. For example a source-native
effect annotation may describe I/O/suspension/allocation/failure, with runtime
ABI validation checking it against the operation; operation selection may not
silently supply a contradictory source contract. Do not claim full declared
contracts while these fields are ignored. Likewise fixed-array construction and
field-dependent lookup results need const/associated/dependent type machinery
or an explicitly modeled compiler type family, rather than fake generic
parameters that callers never bind.
[Current generic syntax](/Volumes/Dev/x/cyallo/crates/cyallo-parser/src/modern.rs:127),
[fixed-array dependent result](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/intrinsic.rs:158),
[field-dependent result](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/intrinsic.rs:126)

### Origine's exact public permission semantics

`origineProvider` include/exclude entries are exact standard-module names without
the `origine:@std/` prefix, for example `text` or `transformer/oxc`; `text` is not
a wildcard subtree. Native `StdFilter::allows` uses equality. The provider first
checks the resolved ID's module name against that filter, then obtains actual
host source and marks it as served. A resolver layer named `origine` only
canonicalizes IDs and grants no permission. `std: true` or omitted options
inherit the creator's own allowed set; a child cannot widen the ceiling.
[Public API](/Users/cyan/Documents/key/nya/origine/src/js/std/graph/providers.js:35),
[exact matching](/Users/cyan/Documents/key/nya/origine/src/api/graph.zig:65),
[native source service](/Users/cyan/Documents/key/nya/origine/src/api/graph.zig:806),
[resolver-only layer](/Users/cyan/Documents/key/nya/origine/src/js/std/graph/resolve/layers.js:21)

Import-map `scopes` are referrer-prefix resolution rules, independent of that
permission filter. Recipe/maker configuration can select the handler governing
a loaded module's own imports. Handler association is keyed by cache namespace
plus resolved module ID; identical path text in another table is another module
identity. This permits policy differences without asserting anything about file
trust. [Public map scopes](/Users/cyan/Documents/key/nya/origine/src/js/std/graph/resolve/layers.js:36),
[recipe handler](/Users/cyan/Documents/key/nya/origine/src/js/std/graph/import.js:150),
[handler identity](/Users/cyan/Documents/key/nya/origine/src/api/context.zig:406),
[import namespace](/Users/cyan/Documents/key/nya/origine/src/api/context.zig:558)

For Cyallo, keep three APIs separate: a source provider returns source plus
loader-issued trust; a resolver maps a request and referrer scope to actual
package/module/declaration identity; an import policy admits that identity under
the scope's ceiling. User configuration may name public paths as ordinary lookup
input, resolved once through declarations into policy sets. The compiler does
not hardcode a list of those paths, and no policy allowlist or filename confers
trusted-attribute authority. Reexports must be checked against their resolved
target so an alias cannot widen the permission set. Granting a module import
still does not mint its runtime capability values.

### Source asset implementation handoff

The follow-up implementation adds the actual [standard entry point](/Volumes/Dev/x/cyallo/std.ya),
24 child `.ya` files, [automatic source packaging](/Volumes/Dev/x/cyallo/crates/cyallo-ir/build.rs),
the [embedded source provider](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/standard.rs),
and a [private semantic ABI registry](/Volumes/Dev/x/cyallo/crates/cyallo-ir/src/internal.rs).
Packaging discovers files and embeds their source text; it contains no public
symbol, export, intrinsic, or signature inventory. The source parse test checks
every embedded asset, every semantic annotation key, and the absence of fake
native function bodies. The complete `cyallo-ir` library unit suite passed
(three tests) after these assets were added. This verifies the assets and
registry; execution verification belongs to the accompanying compiler migration.

The source files now require these explicitly declared native type families:

| Family ABI | Arguments | Evaluation |
| --- | --- | --- |
| `range_array_type` | One value parameter | A fixed array whose element type and length derive from the actual constant range. |
| `empty_array_type` | One element type | A zero-length fixed array of that type. |
| `field_type` | Owner type/value and constant field value | The actual declared field type. A path owner is unwrapped before projection. |
| `element_type` | One collection type | The concrete collection element type. |
| `target_type` | One owner type | Managed/Cell target, or the managed owner retained inside a Weak. |
| `allocated_rc_type` | Element type and allocator value parameter | RC owner retaining the allocator's actual borrowed/owned region relation. |
| `weak_of_type` | One RC owner type | Weak ownership retaining that entire managed owner type, including its region. |

Additional declared semantic bounds make headers truthful without adding public
path recognition: `Cloneable` excludes allocation ownership and generators;
`RcOwner`, `WeakOwner`, and `GcOwner` permit free ownership operations to preserve
the entire actual type; `Lengthable` and legacy numeric bounds retain the exact
compatibility callable domains. `Collection<T>` supplies an element binding;
`NumericCollection<T>` additionally restricts the shapes accepted by array sum.
These source traits require validated semantic predicate implementations in the
shared native declaration environment.
