# Shapes, records, objects, and traits — design draft

Status: proposal for discussion, 2026-11-11. The syntax below is illustrative;
it is not a claim that these examples run in the current interpreter.
The module/service stages 1–4 are implemented; see [the shipped reference](services.md).
The remaining object/shape and optimization features below remain proposals.

## 1. Language model

Separate the type the compiler knows, the storage of a value, and how a member
is resolved. A trait object is a dispatch view, not another kind of field storage.

| Construct | What the compiler knows | Runtime representation | Member access |
| --- | --- | --- | --- |
| `record Point` | Exact nominal type and all fields | Fixed layout, move semantics | Compile-time field offset |
| `(i64, string)` | Exact ordered element types | Fixed aggregate, move semantics | Compile-time element offset |
| `object { x: f64, .. }` | Required typed fields; other fields may exist | Reference to a shape-bearing object | Known result type, guarded slot lookup |
| `dynamic` | An erased owned value; an object may have arbitrary fields | Tagged value; objects retain their shapes | Checked property/operator/call lookup |
| `dyn Logger` | Exact trait interface; unknown implementation | Data reference plus dispatch-table identity | Compile-time method slot, runtime target |
| `Context<Services>` | Service names and types described by `Services` | Resolver-backed open object | Scoped binding resolution with cached dependencies |

The middle ground is an **open typed object**: its complete shape may change,
while a declared subset of members retains checked types. `dynamic` is an erased
view of values, not a second implementation of object storage.

Names such as `object`, `dynamic`, and `borrow` are proposed spelling choices.

## 2. Example code

### Fixed records, methods, and operator overloads

```text
record Vec2 {
    x: f64
    y: f64
}

impl Vec2 {
    fn length_squared(self: &Self) -> f64 {
        return self.x * self.x + self.y * self.y
    }

    proc translate(self: &mut Self, dx: f64, dy: f64) {
        self.x += dx
        self.y += dy
    }
}

impl Add<Vec2> for Vec2 {
    type Output = Vec2

    fn add(self, rhs: Vec2) -> Vec2 {
        return Vec2 { x: self.x + rhs.x, y: self.y + rhs.y }
    }
}

let mut a = Vec2 { x: 1, y: 2 }
let b = a + Vec2 { x: 3, y: 4 }
a.translate(10, 0)
std::io::print(b.x, b.y, b.length_squared())

a.z = 3             // compile error: Vec2 has no z field
```

`impl` methods and operator implementations do not add instance fields.
`a + b` resolves to the selected `Add` implementation during checking when
the operand types are known. Overloaded operators retain their declared
effects; using operator syntax does not imply purity.

`self: &Self` means read-only access through that borrow; `self: &mut Self` permits
mutation. Neither spelling, by itself, proves that unrelated aliases cannot
change storage. Optimizations must use the actual alias/effect analysis.

### Traits: generic and erased dispatch

```text
trait Logger {
    proc info(self: &mut Self, message: string)
}

record ConsoleLogger {
    prefix: string
}

impl Logger for ConsoleLogger {
    proc info(self: &mut Self, message: string) {
        std::io::print(self.prefix, message)
    }
}

proc announce<L: Logger>(ref logger: L) {
    logger.info("started")
}

proc announce_dynamic(ref logger: dyn Logger) {
    logger.info("started")
}

let console = ConsoleLogger { prefix: "app: " }
announce(console)                 // instantiate for ConsoleLogger
let erased = console as dyn Logger
announce_dynamic(erased)          // vtable dispatch; eligible for devirtualization
```

The owned conversion `record_value as dyn Trait` moves the value into an erased object.
A separate borrowed conversion produces `borrow dyn Trait` without a new owned
box and cannot outlive its source. Converting an existing reference object to
a trait object preserves its identity instead of copying its fields.

For the first implementation, vtable-dispatchable methods have a receiver and
a fully known erased call signature. Generic methods are available to static
calls, but are excluded from erased dispatch. Associated types used by erased
methods must be fixed in the trait-object type. Implementation selection is
coherent: there must be one applicable implementation for a trait/type pair.

### Open typed objects and dynamic values

```text
type Named = object {
    name: string
    ..
}

let item = object { name: "Ada", score: 10 }
let mut named: Named = item

named.name = "Grace"              // checked STRING write
named.active = true               // extension property
std::io::print(item.name)                  // same object identity

let mut raw: dynamic = named
let key = "score"
std::io::print(raw[key])                   // dynamic result and runtime checks

raw.name = 42                     // runtime type error: declared field contract
delete raw.name                  // runtime error: required member
delete raw.active                // allowed: extension property

let mut bag = dynamic { score: 10 }   // no required typed members
bag.score = "ten"                // allowed: representation/shape transition
delete bag.score                 // allowed
```

An inferred object literal establishes its initial typed members as required.
An explicit `dynamic { ... }` literal establishes configurable dynamic members.
Subsequent undeclared properties occupy an extensible tail. A field added by
assignment remains dynamically typed unless an explicit declaration establishes
a stronger contract. Unknown literal properties on an open view have type
`dynamic`; computed keys also produce `dynamic` unless a finite key type proves
a more precise result.

Required-member descriptors are enforced through every alias, including a
`dynamic` alias. Erasure cannot bypass these contracts. A dynamically constructed
object can acquire a typed view only through a checked operation that establishes
the required contracts for all aliases; a check of its current contents alone
is insufficient. Read-only shape tests can instead refine facts temporarily,
subject to invalidation by mutation.

Properties on open/dynamic objects use exact string spelling. Lexical names and
nominal-record fields use case-sensitive identifiers.
Conversion between the two must detect ambiguous names rather than silently
merge them.

### Cordis-style contexts

```text
type AppServices = object {
    logger: dyn Logger
    ..
}

let ctx = std::service::Context::new<AppServices>()
let registration = ctx.provide("logger", console as dyn Logger)

ctx.inject(["logger"], proc(scoped) {
    scoped.logger.info("ready")
})

registration.dispose()
ctx.logger.info("gone")           // missing/inactive binding error
```

`Context` and its lifecycle operations are a proposed library abstraction using
a resolver-backed object protocol. Their names are not compiler keywords.
`AppServices` describes the name-to-type relation; it does not claim that every
service is permanently present as a physical field of the context.

A known key such as `"logger"` selects `dyn Logger` at compile time. A computed
unrestricted string selects a checked dynamic result. `inject` grants a scoped
view with the required services; the resolver still observes lifecycle and
isolation rules. Any stronger guarantee that permits eliminating those checks
requires a lifetime/pinning contract from the resolver.

## 3. Runtime shapes and storage

Use distinct nominal type IDs and runtime shape IDs. Two nominal records with
identical fields remain different language types; compatible physical layouts
may still share implementation metadata.

The following Rust sketches describe responsibilities, not final Rust types:

```rust
struct RecordLayout {
    nominal_type: TypeId,
    fields: Box<[FieldLayout]>,
    size: u32,
    alignment: u32,
    trace: TraceDescriptor,
}

struct Shape {
    id: ShapeId,
    fields: Box<[PropertyDescriptor]>,
    lookup: PropertyLookupPlan,
    storage_layout: LayoutId,
}

struct PropertyDescriptor {
    key: PropertyId,
    location: FieldLocation,
    contract: PropertyContract, // required type, or configurable dynamic field
    representation: FieldRepresentation,
}

struct Object {
    shape: ShapeId,
    storage: ObjectStorage,
}

enum PropertyLookupPlan {
    SmallLinear,
    SortedIds,
    SeededPerfect { seed: u64, table: Box<[LookupEntry]> },
    General(GeneralPropertyIndex),
}
```

Record scalar fields occupy fixed offsets, with typed managed-reference fields
described by tracing metadata. Open objects begin with contiguous tagged slots;
later layout versions can unbox fields when their representations are guarded.
Neither representation requires a separately allocated cell for every field.

Shape descriptors are immutable. Adding/deleting an extension property or
changing its physical representation transitions to another shape. Writing a
different value of the same representation need not change the shape. Published
shape IDs are not reused within their identity domain. Holes/deletions and
property enumeration order are recorded explicitly.

Records remain values. A record copy copies its value fields recursively;
object handles inside a record copy preserve the referenced objects' identities.
Scalar replacement can keep a nonescaping record entirely in registers.

### Property IDs and custom lookup

Intern literal property names once. `obj.logger` contains a `PropertyId` in its
bytecode, not a string to hash on every execution. Computed strings require
canonicalization or content lookup; that cost remains until an optimization
actually removes or caches it.

Choose each shape's lookup plan using its finite key set and observed use:

- Very small shapes can compare a few property IDs directly.
- Stable shapes can search for a cheap collision-free seed over their IDs.
- String lookup plans can select discriminating lengths/bytes.
- Large or frequently changing property sets retain a bounded general index.

A perfect hash still validates the candidate key before returning a slot:

```text
candidate = table[hash(key, seed)]
if candidate.key == key { return candidate.location }
return missing
```

Unknown names may collide with known names. Candidate equality cannot be omitted
unless prior proof establishes membership. Hash-plan generation has a bounded
work budget and is amortized across objects sharing that shape; it does not run
on every property insertion. Plans may be upgraded without changing the shape's
semantic field mapping, using immutable replacement plan metadata.

## 4. Compiler lowering, ICs, and traits

Keep the semantic operations visible in typed IR:

```text
LoadRecordField   dst, record, FieldId
StoreRecordField  record, FieldId, value
GetProperty      dst, object, PropertyId, IcSiteId
GetPropertyKey   dst, object, key, IcSiteId
SetProperty      object, key, value, IcSiteId
CallTrait        dst?, receiver, TraitId, MethodSlot, args, IcSiteId
ResolveMember    dst, receiver, PropertyId, IcSiteId
```

An effectful resolver operation cannot become an ordinary field load merely
because both use dot syntax. Record fields are checked and assigned offsets
before execution. Known members of open structural types have checked result
types, but their offsets can differ between concrete shapes.

Each property/call site has feedback separate from immutable program bytecode:

```text
uninitialized -> monomorphic -> small polymorphic -> megamorphic
```

Start with at most four local cases; tune the threshold with measurements.
Cases store shape identities and access handlers. Computed-key cases guard both
the receiver shape and the key. Missing-property results may be cached, with the
same invalidation requirements as successful lookup.

The interpreter records feedback. Winch emits a native IC fast path with a
shared miss helper. Cranelift consumes immutable feedback snapshots and emits
guarded direct loads/stores/calls. A successful monomorphic property access is
approximately a shape comparison followed by an offset load, subject to the
chosen object-reference representation. It does no property hashing.

A trait-object view is conceptually:

```rust
struct TraitObject {
    data: ManagedReferenceOrScopedBorrow,
    dispatch: DispatchTableId,
}
```

Method slots are fixed by the trait interface. The dispatch table identifies
implementation routines through stable entry slots so tier upgrades can replace
native code without changing trait-object semantics. It also describes the
receiver ABI. Tables contain no unrooted Session heap pointers.

Generic calls use a checked implementation selection and monomorphized body.
Erased calls start with table dispatch. If the compiler proves the concrete
implementation, it can call directly without a speculative guard. Otherwise,
an IC can guard a dispatch-table identity and call/in-line the observed target,
with ordinary table dispatch as the fallback. Instance data is still supplied
from the actual receiver; a common target does not imply common receiver values.

Pure trait methods declare `fn`; effectful methods declare `proc`. An
implementation must satisfy the declared effect contract, including borrowed
receiver/capture behavior. The caller must not infer purity from the current
implementation while allowing a future effectful implementation into that slot.

Static type/layout/trait metadata can be shared with the Program. Initially,
dynamic shape identities, IC feedback, and resolver versions belong to the
Session's metadata domain. Compiler workers receive immutable snapshots and
produce code qualified by that domain. Installation occurs on the execution
thread, with domain guards preventing use against unrelated Session shapes.
The current one-entry-per-routine tier table becomes a stable routine dispatcher
plus bounded version entries; a tier upgrade and a shape specialization are
separate dimensions. Sharing dynamic layouts across Sessions can come later
without changing the language contract.

## 5. Cordis resolver caching

The inspected Cordis implementation has materially richer semantics than an
object's physical field map:

- `Context` installs a property proxy and derives isolation/intercept views in
  [context.ts](/Volumes/Dev/x/cordis/packages/core/src/context.ts).
- `locate` walks provision/injection state while respecting service isolation in
  [reflect.ts](/Volumes/Dev/x/cordis/packages/core/src/reflect.ts:83).
- The property handler can invoke accessors, `internal/get` hooks, and traceable
  receiver wrapping in
  [reflect.ts](/Volumes/Dev/x/cordis/packages/core/src/reflect.ts:105).
- Provision/disposal notifications update dependent views in
  [reflect.ts](/Volumes/Dev/x/cordis/packages/core/src/reflect.ts:213).

Therefore, caching only `(object shape, property)` would be incorrect for this
use case. Cache a **resolution handler/binding**, with all assumptions that make
it valid, rather than permanently cache the returned service value.

An initial handler can guard:

```text
receiver's resolution-view identity
property key
resolution-view generation (isolation, injection, shadow/parent changes)
binding identity and lifecycle generation
accessor/hook policy generation
```

These are proposed native-runtime versions; the existing Cordis code does not
already expose this cache contract. The focused
[service/module refinement](services-and-modules-draft.md) prioritizes stable
typed resolution slots and targeted per-service updates over coarse registry
epochs. Provider replacement updates a slot's contents without requiring
whole-function deoptimization. Invalidation must be published before observable
reentrant callbacks.

On a hit, load the binding's current value and perform any remaining observable
policy/wrapping work. Eliminate hooks, checks, or wrapping only with proof or a
guarded dependency. Binding replacement, disposal, activation changes, isolation,
and resolution-policy changes invalidate the corresponding assumptions.

For an injected view whose binding is pinned through its lifetime, the compiler
can potentially hoist resolution and specialize the subsequent trait call.
Pinning keeps the binding valid; it does not automatically make its mutable
service value or methods constant.

## 6. Specialization, speculation, and lifetime decisions

Use two forms of specialization:

1. **Static instantiation:** generic type arguments and selected trait
   implementations determine a body without speculative type assumptions.
2. **Profile-guided versions:** observed shapes, field representations, dispatch
   tables, and closure code targets select guarded native versions.

Version only facts that affect generated code. Set an initial limit of four
speculative versions per routine plus a general version, with a separate global
code-size/compilation budget. Apply admission budgets to static instantiations
too; if admission is exhausted, retain a checked shared/dictionary-based generic
execution path rather than create unbounded JIT code.

Hints can seed expected shapes/types, encourage prewarming, or request a closed
layout. A performance hint cannot remove a required guard. A checked type/layout
contract can establish facts strong enough to remove one.

Begin with speculation that preserves canonical VM state at side exits. Before
an assumed operation, check its conditions. On failure, materialize the exact
live state and resume baseline execution at the appropriate bytecode PC, with
unchanged error ordering and instruction-budget accounting. Never repeat an
already-observed side effect. Hoisted guards, inlining, unboxing, and allocation
elision need recovery metadata for any state they stop storing canonically.

Do not propagate shape/type/constant assumptions across aliasing writes or
effectful calls unless their dependencies remain valid. A shape guard does not
prove field-value constness. Representation guards, value epochs, or watchpoints
must cover additional assumptions.

Borrowed closures and borrowed trait objects carry their source lifetime through
all enclosing composites. Erasing one into owned `dynamic`, an owned trait box,
or an escaping object cannot erase that lifetime. Such escapes are rejected.
Default value captures retain their environment independently of the creating
invocation; explicit `ref` captures borrow existing storage.

Field borrows are logical locations, such as `(rooted owner, FieldId)`, rather
than raw pointers retained across GC or layout transitions. An active borrow
prevents deleting or retyping its target field. Collection can relocate the
owner and repair its registered reference. Native code reloads addresses after
collecting helpers, as it does for the current VM storage.

Shapes and layouts carry precise tracing metadata. Specialized unboxed storage
must be traceable and recoverable before any collecting/reentrant operation.
Feedback and compiled dependencies use stable metadata identities, not raw heap
addresses. Session-local shape IDs must be qualified by their identity domain;
they cannot be compared blindly across Sessions sharing a Program.

## 7. Implementation order and acceptance evidence

For the Cordis use case, the
[service-first implementation order](services-and-modules-draft.md#implementation-order)
takes priority: module imports, typed extension access, and stable service slots
can precede the general open-object shape machinery below.

| Step | Change | Evidence required |
| --- | --- | --- |
| 1 | Intern semantic type/layout IDs; fixed records, tuples, methods | Exact typing, copy/alias rules, operator selection, source diagnostics |
| 2 | Traits, coherent implementations, static generics, erased tables | Generic/erased execution parity; effect and borrow checks |
| 3 | Object arena, immutable shapes, typed required members, dynamic tails | Aliases cannot violate contracts; transitions and GC relocation preserve values |
| 4 | Custom shape lookup plans and interpreter feedback | Unknown-key rejection, collision tests, bounded plan construction |
| 5 | Winch property/call ICs and resolver handler caching | Hit/miss/transition parity; isolation, replacement, disposal, and reentrancy tests |
| 6 | Cranelift versions, guarded devirtualization, side exits | Guard failures preserve values, effects, errors, fuel, and suspended frames |
| 7 | Unboxed fields, scalar replacement, allocation elimination | Recovery/GC stress; lower measured allocations and lookup costs |

These changes touch AST/type metadata, declarations/checking, IR/bytecode,
runtime arenas/tracing, native ABI/emission, tier admission/version dispatch,
and reporting. The current composite implementation reuses array cell storage;
the packed layouts here require an explicit migration rather than an assertion
that those representations already exist.

Add counters for IC hits/misses and state changes, lookup-plan cost, emitted
versions/code bytes, guard exits, resolver invalidations, allocations, and GC
work. Benchmark fixed-field access, monomorphic/polymorphic/megamorphic access,
computed strings, shape churn, trait calls, and service replacement separately.
No speedup is assumed until measured against the current interpreter/JIT.

## 8. Decision ledger

| Decision | Draft choice |
| --- | --- |
| Record openness | Closed, nominal, fixed fields; methods/operators through `impl` |
| Middle type | Open structural object with checked required members |
| Dynamic values | Owned type-erased view; same object storage and contracts |
| Object assignment | Shared identity; record/tuple assignment retains value semantics |
| Member spelling | Exact for open/dynamic property keys; existing case folding for records/lexical names |
| Missing dynamic member | Source-linked runtime error initially; no implicit `undefined` |
| Arbitrary object prototypes | Outside the initial design; resolver behavior is explicit |
| Trait dispatch | Static instantiation where known; `dyn` tables where erased |
| Operator overloads | Trait implementations; fixed records gain no dynamic fields |
| IC placement | Per-site feedback; native guards/handlers; bounded polymorphism |
| Hashing | Shape-specific lookup plans with candidate verification; IC hits bypass hashing |
| Speculation | Guarded versions with correct side exits and bounded code growth |
| Hints | Advisory unless backed by an enforced contract |
| Cordis-like lookup | Typed resolver handlers with scope/lifecycle/policy dependencies |
| Stable service optimization | Stable typed view/key slots; specialize implementation targets, retain current-provider loads |
| Imported extensions | Static lexical resolution; getter may perform dynamic service lookup; import does not activate a provider |
| Captures | Value by default; explicit nonescaping reference captures |
| Dynamic erasure of borrows | Cannot extend lifetime or create an owned escape |
| Borrow alias rules | `borrow` is read-only through that view; `ref` may alias, preserving existing BYREF behavior; exclusivity requires proof |
| GC pressure | Contiguous fields, explicit tracing; later scalar replacement and environment elimination |

The main remaining language choices are the final spelling of structural object
types and borrowed views, and the general resolver-protocol declaration syntax.
The runtime/optimizer design must not silently assume Rust-style exclusivity;
this draft deliberately preserves aliasing reference parameters.

## References

- [V8: hidden classes and inline caches](https://chromium.googlesource.com/v8/v8/+/bcefebe8498628369427a91063982fc0160da99f/docs/runtime/hidden-classes-and-ics.md): shape-based handlers and site feedback.
- [Typed object shapes and basic block versioning](https://arxiv.org/abs/1507.02437): representation specialization and propagation of shape knowledge.
- [GNU gperf](https://www.gnu.org/software/gperf/manual/html_node/Description.html): finite-key lookup generation and selected-byte hashing.
- [Rust trait objects](https://doc.rust-lang.org/reference/types/trait-object.html) and [dyn compatibility](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility): erased interface dispatch and representable method signatures.
- [Rust compiler monomorphization](https://rustc-dev-guide.rust-lang.org/backend/monomorph.html): static concrete instantiation and code-size tradeoffs.
- [Existing runtime architecture](architecture.md) and [GC plan](gc-plan.md): canonical frames, explicit roots, relocatable references, and current implementation limits.
