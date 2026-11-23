# Removing `StorageKind`

Status: plan, 2026-11-17. Step 3's first family, `Clock`, is implemented (see "Progress");
the rest is not. It follows the
[provider bindings](sdk-registration.md) and [intrinsic contracts](intrinsic-contracts.md)
work and replaces the rejected model in `docs/pending-work.md`.

## What exists

`StorageKind` (`crates/cyallo-ast/src/lib.rs`) has 27 live variants and is carried as
`Type::Storage(kind, inner)`. A measured count (2026-11-17) is about 330 uses in 35 files:

| Area | Files (uses) |
| --- | --- |
| Native contracts, declarations, members | `native_contract.rs` 51, `declarations.rs` 49, `member.rs` 41, `internal.rs` 29 |
| Names and types | `display.rs` 35, `namespace.rs` 26, `enums.rs` 27, `lookup.rs` 24, `intrinsic.rs` 19, `inference.rs` 7 |
| Ownership, memory, GC | `ownership.rs` 17, `memory.rs` 11 |
| Analysis | `analysis.rs` 11, `composites.rs` 10, `generator.rs` 7 |
| Runtime | `src/runtime.rs` 11, `src/dynamic_modules.rs` 4, `cyallo-values` 4, and a few in `src/runtime/*` |

By variant (uses/files): `Vec` 32/16, `VecDeque` 24/14, `Result` 22/14, `Weak` 20/10,
`Option` 20/10, `StoragePath` 18/8, `Range` 14/9, `ReceiveError` 13/7, `GeneratorStep` 13/9,
`Sender` 12/8, `SendError` 12/8, `Task` 11/8, `PureGenerator` 11/6, `Generator` 11/7,
`Clock` 11/8, `Receiver` 10/7, `Location` 10/7, `Cell` 10/7, `Allocation` 10/6, `Layout` 9/7,
`LoadError` 8/7, `ViewError` 7/6, `TypeDescriptor` 7/6, `ParseError` 7/6, `AllocError` 7/5,
`TaskOutcome` 6/4, `ArenaCell` 5/5.

## What the type system uses a kind for today

1. **Identity and printing**: "this type is a `Clock`", `Display`.
2. **Result types of native members**: `member.rs` builds `storage(K::TaskOutcome, payload(r, K::Task))`
   by kind, duplicating what the declaration already says.
3. **Ownership, transfer and GC**: copy vs move, `transferable`, tracing, drop effects.
4. **Receiver checks**: `payload(r, K::Task).is_some()` decides whether a member applies.
5. **Runtime values**: `Value::Optional`, `Value::Result`, `Value::SendError`, ...

## Target

Library types are ordinary nominal types. The compiler learns nothing about a type from a
name or kind:

- **Identity** is the declaration (`Type::Named`/`Applied` of its resolved target). Printing uses
  the declared name.
- **Result types** come from the instantiated declaration signature. `native_contract.rs`
  already instantiates and validates it; the by-kind synthesis in `member.rs` and
  `lookup.rs` is deleted, not ported.
- **Ownership, transfer and GC** consult the type's declared contract. Opaque host types
  carry an `OpaqueContract` (copyable, transferable, drop runs guest, holds managed
  references) supplied by the provider's registration; data types use their fields.
- **Receivers** are checked by the declared signature (`self: &Clock`), not by kind.
- **Runtime values** for data types are `Value::Enum`/records with the declared variant ids;
  the runtime obtains declared variants and records through provider bindings (a
  constructor handle bound to `std::channel::ReceiveError::Closed`), not from enum
  encodings. Opaque host types keep their own runtime representation.

Three families, three paths:

| Family | Types | Path |
| --- | --- | --- |
| **Data** | `Option`, `Result`, `GeneratorStep`, `SendError`, `ReceiveError`, `LoadError`, `ViewError`, `ParseError`, `AllocError`, `TaskOutcome`(+failure) | Plain source enums/records with source-bodied helpers; runtime produces declared variants; a generic enum layout optimizer replaces the Option/Result special layout |
| **Host resources** | `Cell`, `Location`, `StoragePath`, `Sender`, `Receiver`, `Task`, `Clock`, `Generator`/`PureGenerator`, `Weak`, `ArenaCell`, `Allocation`, `Layout`, `TypeDescriptor` | Opaque records with an `OpaqueContract`; operations are bound intrinsics/host calls |
| **Collections** | `Vec`, `VecDeque`, `Range` | Source over buffer intrinsics where expressible, otherwise opaque with a contract |

## Order

Each step leaves the tree building and the baseline test set unchanged.

1. **Opaque contracts in the provider table.** Extend the bindings with an `OpaqueContract`
   per opaque type; `ownership.rs`, `concurrent::transferable` and `memory.rs` read it
   through the declaration instead of matching kinds. (No representation change yet.)
2. **Declared result types.** Replace by-kind synthesis in `member.rs`/`lookup.rs`/`concurrent.rs`
   with the instantiated declaration signature.
3. **One leaf family end to end** to prove the mechanism, then the rest of its class:
   `Clock` (opaque, no payload), then `ReceiveError` (data, needs runtime constructor
   handles).
4. **Data enums**: variant handles for the runtime, then `Value::Enum` everywhere,
   delete `EnumEncoding`, `Value::Optional`/`Result`/`SendError`/`ReceiveError`/
   `GeneratorStep`, add the layout optimizer.
5. **Host resources** one by one (`Task`, `Sender`/`Receiver`, `Generator`, `Weak`, ...).
6. **Collections**.
7. **Delete** `StorageKind`, `Type::Storage`, `NativeType::Storage`, `display.rs`
   special cases and the `declarations.rs` kind lists; shrink `abi_keys!` to engine
   operations only.

## Risks

- The generator and task paths are coupled to interpreter frames and the scheduler; their
  contracts must be exact before the types change.
- `Vec`/`VecDeque` are the hottest paths (32 and 24 uses): migrate only with benchmarks
  against `docs/benchmarks.md`.
- The 15 failing tests and `enum_resolution` are caused by the migrated enums appearing as extra
  functions; they should clear when step 4 lands. Verify rather than assume.
- A wrong contract is unsound; keep the debug conformance assertion and GC-stress runs in
  every step.

## Progress

### `Clock` (2026-11-17): first family without a kind

`StorageKind::Clock` is deleted. A clock is a `Type::Opaque(target, arguments, OpaqueTraits)`:

- **Identity** is the resolved declaration target. Nothing asks whether a type is "a clock";
  a user `record Clock {}` is a different type, and renaming `std::time::Clock` (source and
  provider table) changes nothing else. `Display` prints the declared name.
- **Traits, not kinds.** `OpaqueTraits { copyable, transferable, effectful, cloneable }` are
  declared by the provider in the binding key: `std::time::Clock opaque:transfer,effectful,clone`.
  `ownership.rs` (lifecycle, borrows), `concurrent::transferable`, `memory.rs` and
  `native_contract.rs` (cloneable) read them from the type. Unknown flags are an unknown key.
- **Result types** come from the declaration. `std::time::clock()` takes its result from the
  instantiated declared signature (`resolve_declared` passes it to the intrinsic), and the
  `ClockClone`/`ClockSleep` members apply to any opaque receiver the declaration names.
- **Generic opaque types** work the same way (arguments are substituted, unified and walked);
  `Clock` has none, so that path is exercised by the type-system plumbing, not yet by a family.

Verification: `tests/native_bindings.rs` gained three tests (declaration-not-name identity,
a same-named user type is distinct, traits come from the binding); the clock example runs on every
engine; the root suite fails the same 15 tests as before. Remaining 26 variants are unchanged.

What this step shows for the rest: a host resource needs (1) a `Type::Opaque` binding key with
its traits, (2) its member/intrinsic result types taken from the declaration, and (3) its
receiver checks relaxed from "is kind K" to "is opaque". Families whose members synthesize result
types by kind (`Task`, `Sender`/`Receiver`) need step 2 done for them first.

### `Sender` and `Receiver` (2026-11-17)

`StorageKind::Sender` and `Receiver` are deleted; both are generic opaque types
(`Type::Opaque(target, [T], traits)`) bound as `opaque:transfer,effectful,clone`.

- The channel constructors take their result pair from the declared signature
  (`resolve_declared` passes the declared tuple, and the element type, to the intrinsic).
- Channel members (`clone`, `close`, `is_closed`, `send`, `try_send`, `recv`, `try_recv`) apply to
  any opaque receiver; the element type is the type argument the declaration applied.
- Several opaque types share one set of traits, so the "canonical native type declared once"
  check no longer applies to opaque declarations.
- Still by kind: `SendError`/`ReceiveError` (the members' `Result` payloads), until the data enums
  move.

### `Task` and `TaskOutcome` (2026-11-17)

`StorageKind::Task` and `TaskOutcome` are deleted. `Task<T>` is bound `opaque:effectful,resource` (a unique
handle: not cloneable, not transferable); `TaskOutcome<T>` is `opaque:clone` (owns only what its argument
owns, cloneable when `T` is).

- New trait `resource`: the value is itself an owned resource whatever its arguments are. Without it an
  opaque type owns only what its arguments own (`potentially_owning`); `copyable` now means "copy when its
  arguments are" (`copy()`).
- `spawn` and `join` take their result types from the declaration (`resolve_declared` passes the declared
  type, `member::specialize` receives the declared result). Because the declaration now chooses the type, the
  contract still validates its shape: a host constructor must declare an opaque type (otherwise
  `InvalidSignature`, E173), `spawn`'s handle must be applied to the body's result, a channel constructor must
  return two ends of the element type.
- Outcome accessors (`completed`, `is_*`) apply to any opaque receiver; `completed` takes the argument.
- Still by kind: the outcome's `Failed` payload is a runtime value, and `TaskOutcome` is not yet a declared
  data enum (that is step 4).

### `ArenaCell`, `Generator`/`PureGenerator`, `Weak` (2026-11-17)

Eighteen variants remain (`Cell`, `Location`, `StoragePath`, `Option`, `Result`, `Vec`, `VecDeque`, `Range`,
`ParseError`, `Layout`, `Allocation`, `AllocError`, `GeneratorStep`, `SendError`, `ReceiveError`, `LoadError`,
`ViewError`, `TypeDescriptor`). Three more opaque traits carry what the compiler genuinely needs:

| Trait | Meaning | Used by |
| --- | --- | --- |
| `indirect` | arguments live behind an indirection; a recursive type may name itself through it | `ArenaCell`, `Weak` (composites, enum recursion check) |
| `generator` | the language drives values with `yield`/`resume`; arguments are (yield, resume, return) | `Generator` (`generator::parts`) |
| `weak` | a weak reference to an `rc` owner; written `Weak<'a, T>` the language builds the owner | `Weak` (namespace, `downgrade`/`upgrade`, `WeakOwner`, `Target`) |

`pure` is set by the compiler, never declared: a generator made by a pure `fn` (formerly `PureGenerator`) is an
opaque generator with `pure` set and is accepted where the general type is expected.

How the language finds a type it needs without a name: it asks for the trait (`weak_type()` looks up the
declaration whose binding has `weak`). The downgrade result, `WeakOf<Owner>` and `Target<Weak>` come from that
declaration or from the declared signature; `RcDowngrade` takes its result type from the declaration like `clock`
and `spawn`. Opaque types now compare region-aware in `same_regions` (allocator lifetimes still matter).

What remains in each family is stated in the table at the top; the next candidates are `Allocation`/`Layout`
(allocator ABI), `Cell`/`Location`/`StoragePath` (managed storage), then the data families (step 4).

### `Cell`, `Location`, `StoragePath` (2026-11-17)

Fifteen variants remain: `Option`, `Result`, `Vec`, `VecDeque`, `Range`, `ParseError`, `Layout`, `Allocation`,
`AllocError`, `GeneratorStep`, `SendError`, `ReceiveError`, `LoadError`, `ViewError`, `TypeDescriptor`.
Three more traits state what the language does with these types:

| Trait | Meaning | Bound on |
| --- | --- | --- |
| `deref` | `.*` yields the single argument; reading and writing a cell go through it | `Cell` |
| `target` | `Target<Owner>` of the type is its single argument | `Cell`, `StoragePath` (and `Weak`, which also has `weak`) |
| `path` | a guarded storage path: a relative lookup plan over its argument; a mapper returns it | `StoragePath` |

`Location` needs no semantic trait: its members apply to any single-argument opaque receiver and `load` yields
the argument. The constructors (`Cell::new`, `lookup::path`, `lookup::missing`, `lookup::field`) take their
result type from the declaration and validate its shape (`traits.deref`/`traits.path`, argument equals the
operand type). Path members (`field`, `element`, `target`) compute the record index or element check from the
receiver and take the result type from the declaration. Recursion through `Location`/`StoragePath` uses the
`indirect` trait.

### `Allocation` and `Layout` (2026-11-17)

Thirteen variants remain: `Option`, `Result`, `Vec`, `VecDeque`, `Range`, `ParseError`, `AllocError`, `GeneratorStep`,
`SendError`, `ReceiveError`, `LoadError`, `ViewError`, `TypeDescriptor`. Two role traits describe the allocator
protocol: `layout` (`Layout`, bound `opaque:copy,clone,layout`) and `allocation` (`Allocation`, bound
`opaque:effectful,resource,allocation`).

- The `Allocator` trait is validated by roles: `allocate(&mut self, layout) -> Result<allocation, AllocError>` and
  `deallocate(&mut self, allocation)`. `AllocError` is still a kind (a data enum, step 4).
- `rc(allocator) value` no longer builds layout and allocation types: it takes them from the allocator trait's declared
  `allocate` signature, so any provider spelling works.
- `Layout::new`/`allocate` take their result types from the declaration (`resolve_declared`); the memory operations
  validate the role traits and the `AllocError` payload.
- `memory::layout()` and `memory::allocation()` are deleted.

### `Vec` and `VecDeque` (2026-11-17)

Eleven variants remain: `Option`, `Result`, `Range`, `ParseError`, `AllocError`, `GeneratorStep`, `SendError`,
`ReceiveError`, `LoadError`, `ViewError`, `TypeDescriptor` (91 uses). Two traits describe collections:
`collection` (an indexable sequence of its single argument: `v[i]`, `len`, slices, `for`, `ArraySum`, bounds like
`Collection<T>`) and `double_ended` (also grows and shrinks at the front). `Vec` is bound
`opaque:clone,transfer,indirect,collection`; `VecDeque` adds `double_ended`.

- **Literals.** `Vec[..]`, `Vec<T>[..]`, `VecDeque[..]`: the resolver recognizes a literal head by the `collection` trait of
  the type symbol and passes the declared type as the first type argument of the literal intrinsic (the second, optional,
  is the element type). The operation builds `Opaque(target, [element], traits)` and checks that `double_ended` agrees with
  the intrinsic.
- **`Vec::from(range)`** takes its result from the declaration and validates it.
- **Members.** `push`/`pop`/`len`/slices test the trait; `push_front`/`pop_front` need `double_ended`; a slice view is only
  offered for a collection that is not double-ended (contiguous storage).
- **Runtime.** Three places asked "is this heap cell a Vec or a deque": they now ask for the `collection` trait, and the
  representation (array or ring) follows the existing value or the `double_ended` trait. No new allocation on the index path.
- **Measured.** Collection workloads are 2-3% slower than the pre-branch baseline, matching an integer-only control, so
  the cost is not specific to collections (see "Collections after `StorageKind` removal" in `docs/benchmarks.md`).

## ReceiveError: first data enum

`std::channel::ReceiveError` is now an ordinary source enum. It has no `StorageKind`, no `receive_error_struct` binding
and no dedicated `Value`/`Payload` variant.

- **Roles.** `Closed` and `Empty` still carry the `receive_closed_variant`/`receive_empty_variant` role tags. The linker
  collects every tagged variant into `NativeDeclarations::variant_roles` (any enum, not only native storage ones) and the
  program keeps the table, so the runtime builds `Value::Enum(variant, None)` for the variants it produces (`recv`,
  `try_recv`) and `is_closed`/`is_empty` compare against the same ids.
- **Types.** `recv`/`try_recv` take their `Result<T, ReceiveError>` from the declaration. A named source enum in a native
  signature is resolved through the program's definitions (`Bindings::substitute` and `unify`), so the enum matches
  user code (`match error { ReceiveError::Empty => .. }`).
- **Self in native impls.** `Self` is substituted with the unresolved impl receiver before the signature is resolved;
  the resolved name cannot be resolved a second time.
- **Verified.** Full root suite fails the same 15 tests as the baseline (names compared); crate tests unchanged except the
  known `enum_resolution` failure; `examples/concurrency/status-enums.yas` runs.
- **Next.** `SendError`, `GeneratorStep`, `Option`, `Result` (these carry payloads and generic parameters, so they also need
  the generic layout optimizer), then the error enums, `Range` and `TypeDescriptor`.

## Option and Result: survey and open design point

Sites to change: about 12 context-free type constructors (`lookup::result_type`, `storage(Option, ..)`, `memory::error`,
`builtins`, `intrinsic`), about 15 matchers on `Type::Storage(Option|Result, ..)` (`lookup::inner`, `payload`,
`memory.rs`, `intrinsic.rs`, runtime `modules.rs`/`dynamic.rs`/`lookup.rs`), and about 60 runtime uses of
`Value::Optional`/`Value::Result` (lookup, memory, gc, concurrent, modules, dynamic).

`ReceiveError` was easy because it has no parameters and the runtime only needed two variant ids. `Option<T>` and
`Result<T, E>` are generic and are built in places that have no environment: a `Type::Storage(Result, (T, E))` is
constructible anywhere, while a real `Type::Enum` needs the identity of the std declaration. Types and values must flip
together, so this is one change, not a series.

Decision needed: how those constructors obtain the std declaration.
1. Thread a `&TypeEnvironment` (or an `Arc<StdEnums>` of role-keyed templates held by it) to every constructor. No global
   state; touches every caller.
2. Keep `Type::Storage(Option|Result, ..)` as the type's spelling and resolve it to the declaration's `EnumType` only at
   layout/runtime boundaries. Small, but leaves the compiler-owned marker this work is meant to remove.

## Option and Result values are core enums (runtime done)

Direction (user, 2026-11-17): `core` is always supplied and holds `Option`, `Result`, numeric constants and struct
utilities; compiler code names its enums by fixed identity instead of threading an environment.

Done:
- The variants of `Option` and `Result` have **reserved declaration ids** (`VariantId::OPTION_SOME/NONE`,
  `RESULT_OK/ERR`; `RESERVED_DECLARATIONS = 16`). The linker assigns them to the role-tagged variants and starts other ids
  above the reserved range, so every program, dynamic child and runtime names them identically.
- `Value::Optional`, `Value::Result`, `Payload::Optional` and `Payload::Result` are gone. Runtime code uses
  `Value::some/none/option/ok/err/result` and `as_option/as_result/into_result`; values are `Value::Enum`. Display,
  GC tracing, remapping, drop, cloning, cross-task transfer and constant freezing use the general enum paths.
- Verified: root suite fails the same 15 baseline tests (names compared); crate tests unchanged.

Remaining for the type side: `Type::Storage(Option|Result, ..)` is still the context-free spelling. Replace it with a
reference to the core enum by fixed identity (reserved `EnumId`s plus canonical instance keys), then delete
`StorageKind::Option/Result`, `EnumEncoding::Option*/Result*` and add the shape-based layout optimizer.

## GeneratorStep, SendError and ReceiveError values are core enums

Same technique as Option/Result. Reserved ids now cover every role: `STEP_YIELDED/COMPLETE`, `SEND_CLOSED/FULL/LIMIT`,
`RECEIVE_CLOSED/EMPTY` (ids 4-10). `Value::GeneratorStep`, `Value::SendError`, `Payload::GeneratorStep` and
`Payload::SendError` are gone; use `Value::step/as_step` and `Value::send_error/as_send_error`. The `variant_roles` table
and `Program::variant_for` added for `ReceiveError` are deleted, since the ids are constants. Cross-task transfer,
GC, cloning, drop and fingerprints use the general enum paths. Root suite: same 15 baseline failures; crate tests unchanged.

Not converted: `LoadError`, `ViewError`, `ParseError`, `AllocError` and `TypeDescriptor` are opaque host payloads rather
than data enums (no variants to match), so they belong with the opaque-type migration, not this one.

## Type marker: findings before the change (2026-11-17)

Nothing was edited for this step; a source snapshot was saved before starting.

- `Type::Storage(Option|Result|GeneratorStep|SendError, T)` is already interchangeable with `Type::Enum`: pattern matching,
  ownership, transfer and constants all go through `enums::contract(ty, native)`, which accepts either form.
- Producers of the marker: `namespace.rs` (annotation `Applied` -> `Storage`), `enums::native_type`,
  `declarations.rs` (`TypeEnvironment::new`), `native_contract.rs` (substitute), `member.rs`, `lookup.rs`, `memory.rs`,
  `generator.rs`, `intrinsic.rs`. About a dozen build it without a `TypeEnvironment`.
- Consumers of `Type::Storage(_, inner)` as a generic payload walker (ownership, memory, gc_metadata, services, display):
  about 25 arms, all with an existing `Type::Enum` equivalent.
- Environment-carrying entry points all pair `composites::resolve_modules` with `TypeEnvironment::new(types, native)`
  (9 callers), so templates for the core enums can live in that map.

Plan: (1) members and intrinsics take Option/Result/Step/Send types from the declared result (the pattern already used
for tasks, receivers and allocators); (2) annotation resolution and native signatures produce `Type::Enum` from the
declaration, with `substitute`/`unify` instantiating templates; (3) delete the four `StorageKind` variants and the role
tags in one sweep. Steps 1-2 need dual-form tolerance during the transition, so each is verified against the baseline
before the next.

### Step 1 done: readers are form-agnostic, member results come from declarations

- `lookup::option_payload` and `lookup::result_payload` read an `Option`/`Result` type in either form (`Type::Storage`, or a
  `Type::Enum` recognised by its role-tagged variants). Member resolution (`is_some`, `unwrap`, `is_ok`, ...), the
  intrinsic specializations, allocator result checks and the runtime's view/allocation/module-load code use them.
- `RcUpgrade` and `CollectionPop*` take their `Option` result from the declaration, validated against the receiver.
- Intrinsic calls already end with `resolved.result = contract.result`; the internally built type is only compared with
  the declared one with `equivalent_types`. That comparison is the bridge step 2 must make form-agnostic.
- Remaining context-free producers: `lookup::constructor`/`call` (some, none, upgrade, mapped), `memory::error`,
  `intrinsic.rs` result types, `generator.rs`, and the generated module-load wrapper in `src/dynamic_modules.rs`.
- Verified: same 15 baseline failures; crate tests unchanged.

### Step 2 done: annotations and native signatures produce the declared enums

- `composites::resolve_modules(asts, native)` now keeps the native enum declarations among its definitions and turns a
  `Type::Storage(Option|Result|GeneratorStep|SendError, ..)` annotation into `Type::Enum` (`enums::contract_for`).
- `TypeEnvironment::new` normalizes the native function and method signatures the same way; type parameters stay as `Named`
  arguments of the template. `Bindings::substitute` and `unify` gained `Type::Enum` arms (arguments and payloads).
- Bridges for the remaining marker producers: `memory::same_regions` and `generics` unify treat the marker and the
  enum as one type (`enums::core_shape`). Step/send readers (`lookup::step_payload`, `send_payload`) join the option and
  result readers; send, step, upgrade and pop members take their result from the declaration.
- Verified: the root suite fails the same 15 baseline tests; crate tests all pass (the earlier `enum_resolution`
  failure no longer occurs).
- Left for step 3: the internal marker producers (`lookup::constructor/call`, `memory::error`, `intrinsic.rs` result
  types, `generator::step`, the module-load wrapper) only feed comparisons against declared types now; delete them and the
  four `StorageKind` variants, the `EnumEncoding` role tags that only the marker needed, and the bridges.

### Constructor functions removed; findings after step 2 (2026-11-17)

- `std::option::some`, `std::option::none<T>()`, `std::result::ok` and `std::result::err` are deleted from `std/`, from
  `std.bindings` and from the docs. `Option::Some`/`Option::None`/`Result::Ok`/`Result::Err` (the enum constructors) are the
  only way to build them; tests and examples were migrated, with annotated `let`s where a bare `None` needs a type. The
  generated module-load wrapper in `src/dynamic_modules.rs` builds its `Ok` with `EnumConstruct` and the reserved id. The
  `OptionSome/OptionNone/ResultOk/ResultErr` intrinsics and runtime operations are now unreferenced from source and still
  need deleting.
- `Type::Enum` holds an `Arc<EnumType>`: cloning a type no longer copies variants and names. Runtime `store` keeps the
  core enums inline in their cell (`enums::is_core`), as the marker types were, instead of giving every write a payload cell.
- Measured on the same program written in the new syntax, release CLI (interpreter / jit): 2268 ms / 1580 ms. The earlier
  helper-function version of the program ran 1907 ms / 1274 ms on this tree and 1954 ms / 1339 ms on the baseline, so
  the enum-typed path costs about 16-18% more than the marker path did; the programs differ (constructor op versus
  intrinsic), so the figures are indicative. Not yet profiled.
- Open defect (not introduced here): matching on a payload-carrying enum allocates a payload cell per match
  (`Op::EnumPayload` with `Projection::Address` on an inline value), and without GC the cells are only reclaimed when the
  frame exits. A loop of about a million matches reaches `max_cells` ("execution storage limit exceeded"); a user-defined
  enum does the same at 3M iterations. A by-value projection for copy payloads would avoid it.
