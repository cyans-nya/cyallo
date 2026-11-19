# SDK registration

Source: [`crates/cyallo-ir/src/registry.rs`](../crates/cyallo-ir/src/registry.rs). Part of the
[component worlds](component-worlds/README.md) work: an SDK declares signatures and an
implementing component supplies the rest. This page describes how the runtime (or an
embedder) binds the SDK's **bodyless** items, using the contracts in
[intrinsic effect contracts](intrinsic-contracts.md).

## Terminology

These terms are used by the [backend design](backend-design.md) and the component
worlds drafts. Use them the same way.

| Term | Meaning |
| --- | --- |
| SDK | A package of worlds and interface modules: signatures only, no bodies. |
| Provider | The runtime or embedder that implements an SDK's bodyless items. It owns its `NativeBindings`. |
| Binding | The contract and implementation for one SDK item path: `host_call`, `intrinsic` or a Cyallo body. |
| Installed table | The immutable `Installed` result of installing a provider's bindings against an SDK interface, scoped to that program and provider. |
| Bound call | A call to an SDK item, resolved through its installed binding. |

Avoid "provider call" or "installed provider" for a bound call or an installed
table. "Provider bindings" in older text means the provider's `NativeBindings`.

The first integration fixture uses the installed table to select a checked
Cyallo reference by declaration ID. The verified interpreter executes the
reference through a direct guest call. The fixture uses two separate bindings
for the same alias, and checks selected results, typed overflow and its source
span. [Scalar reference fixture](../tests/sdk_reference.rs)

The synchronous host fixture binds a bodyless `proc` to an embedder callback.
It verifies one callback invocation across one-instruction budget slices,
terminal failure at the call site, and isolation between installations.
[Scalar host fixture](../tests/sdk_host.rs)

## What needs binding

`InterfaceItem::requires_binding` (from `ResolvedProgram.interface`, see
[`sdk.rs`](../crates/cyallo-ir/src/sdk.rs)) is true for:

- a bodyless function or method declared in an `interface mod`;
- an `opaque record`.

Items with a Cyallo body, transparent records, enums, traits and aliases need no
binding. `requires_binding` and `arity` are not part of the digest: bodies are
implementation.

## The three kinds of implementation

| Registration | Binds | Contract | Notes |
| --- | --- | --- | --- |
| `host_call` | A function or method | `BindingContract` with disposition `HostCall` | Opaque to the optimizer; declared effects and safepoint |
| `intrinsic` | A function or method | `BindingContract` with disposition `EnginePrimitive`, a **reference**, optional lowerings | The reference (a Cyallo or declarative body) defines the meaning and is the checked fallback; lowerings (`Winch`, `Cranelift`, `CraneliftSpeed`) are declared, never trusted beyond the contract |
| `opaque_type` | An opaque record | `OpaqueContract`: copyable, transferable, drop runs guest code, holds managed references | What the compiler and collector need to know about the representation |

`BindingContract` is `MemberContract`: disposition, safepoint, effects and operand passing
per position (`read`, `move`, `&`, `&mut`).

## Using it

```rust
let sdk = /* ResolvedProgram.interface */;
let installed = Registry::new()
    .opaque_type("tasks::Handle", sdk_digest("tasks::Handle"), opaque_contract)
    .host_call("tasks::spawn", sdk_digest("tasks::spawn"), spawn_contract)
    .intrinsic("tasks::sum", sdk_digest("tasks::sum"), sum_contract,
               Some(Reference::Source("sum_reference".into())), vec![Engine::Cranelift])
    .install(&sdk)?;            // Result<Installed, Vec<RegistrationError>>
installed.contract("tasks::sum");  // what the compiler may rely on
```

A `Registry` is a plain value owned by one runtime or session. Two registries may
bind the same item differently. `install` collects registration errors while checking
bindings against an SDK interface.

For a resolved program, `Registry::install_for_program` also links each checked
contract to its resolver-assigned `DeclarationId`. The returned `ResolvedBindings`
is immutable and scoped to that program and provider. Imports preserve the original
declaration ID, so an alias reaches the same contract without another path lookup.
Linking fails if an item has no declaration or does not identify a single declaration.
The execution path still uses legacy provider bindings while this interface is adopted.

`VerifiedModule::lower_sdk_reference` accepts a closed Modern source bundle
from an embedder and installs its registry against that exact resolved graph.
It links bodyless free functions to qualified source references with matching
`i64` or `bool` signatures. This slice supports value parameters, read-only
operands, synchronous execution, and declared arithmetic failure. The normal
semantic checker checks the reference bodies. Verified lowering rejects any
reachable operation outside the interpreter's supported set. Each compiled
fixture retains its own selected implementation. [Reference linking](../crates/cyallo-ir/src/registry/reference.rs) · [Verified fixture compilation](../src/bytecode/verified/sdk.rs)

`ScalarSdk::host_call` registers the contract, SDK digest and callback together.
`VerifiedModule::lower_sdk` links that registration against a closed Modern
bundle. The verified call-site target records the installed host slot.
The verifier checks its argument and result types, and the interpreter passes
copied scalar values to the retained callback. This slice supports fixed
`i64` and `bool` procedure signatures with a scalar result, read-only value
operands and `Safepoint::Never`. The host can return a typed rejection if its
contract declares `may_fail`. A bad return type, unexpected rejection or
unwinding callback panic terminates the activation at the bound call's PC.
An unused scalar result has no destination. The interpreter still calls the
host and validates its result or reports its failure.
[Scalar host API](../src/bytecode/verified/bound.rs) · [Interpreter execution](../src/bytecode/verified/run.rs)

## What installation checks

| Check | Error |
| --- | --- |
| The path names an SDK item | `UnknownItem` |
| The item has no body in the SDK | `NotBindable` |
| The item's kind fits the registration (routine, or opaque record) | `KindMismatch` |
| The digest the implementer was built against equals the SDK's | `DigestMismatch` (names both digests) |
| The item is bound once | `Duplicate` |
| Every bindable item is bound | `Unbound` |
| The contract is consistent | `Contract(ContractViolation)` |

Contract violations: parking or calling guest code needs a `MayReenter` safepoint;
allocation cannot be `Never`; a host call must have the host-call disposition; an
intrinsic must have the engine-primitive disposition and a reference; operand
exceptions must refer to existing parameters. The same rules are enforced for the engine's
own intrinsics by tests in `contract.rs`.

## What is not built yet

- Default execution still resolves native items through the legacy private key path.
  Checked lowering and runtime dispatch need to consume linked provider contracts.
- General reference resolution and conformance still need declarative bodies,
  world loading, native lowerings, and host effects that park, collect, or
  reenter guest execution.
- `Engine` lowerings are declared but there is no emitter API to supply them.
- The digest is supplied by the implementer from the SDK artifact; the load handshake and
  use-set checks from the drafts are not implemented.
- Opaque type contracts are recorded, not yet consulted by ownership, transfer or GC.

## First consumer: provider bindings in the linker

The compiler now takes native bindings from the provider instead of from the SDK source.
[`bindings.rs`](../crates/cyallo-ir/src/bindings.rs) defines `NativeBindings`: SDK item path →
engine semantic key (`std::cell::Cell` → `cell_struct`, `std::cell::Cell::new` →
`cell_new`). A `CorePackage` carries one (`with_bindings`; the embedded package carries
`NativeBindings::embedded()`), and the linker applies it to the provider's own source
before resolution, so the rest of the pipeline is unchanged. User source never receives
bindings.

- **Every standard family is migrated.** 227 bindings (functions, types, enum variants,
  traits, members) live in [`crates/cyallo-ir/bindings/std.bindings`](../crates/cyallo-ir/bindings/std.bindings),
  one `path key` per line. The `std/*.ya` files carry no `#[cyallo_internal]` annotations;
  the only one left is on the `prelude` module in `std.ya` (module annotations are not
  bindable yet).
- **Paths are the identity.** An item path is the declaring module path, the item name,
  and for methods the owner and the method name. A nominal receiver is its name
  (`std::option::Option::is_some`); another receiver is its spelling
  (`std::array::[]T::len`, `std::gc::gc T::clone`). Renaming or moving a declaration
  changes its path, so its binding is edited in the table; the compiler is not touched.
  A binding whose path does not exist is `UnusedBinding`; a declaration with both an
  annotation and a binding is `BindingConflict`; an unknown key or wrong kind is rejected
  exactly as an annotation would be. Two bindings of one path (a method repeated in two
  impl blocks) must agree.
- **Adapting a provider.** `NativeBindings::map` rebuilds the table entry by entry, which
  is how tests move, rename or unbind declarations. A host that copies or rewrites the
  embedded sources must pass its bindings through `CorePackage::with_bindings`; runtime
  `std::module::load` links against the host package's own bindings.
- **Diagnostics.** A binding has no annotation text, so an error about it points at the
  bound declaration.
- **Compatibility path.** The provider owns these keys. The embedded standard bundle
  still uses them while migration adds interface digests and adopts `Registry` contracts.
  Keys still map to `StorageKind`, which will be removed as each declaration family migrates.
