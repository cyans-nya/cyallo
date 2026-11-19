# Runtime SDKs, worlds, and provider bindings

This is the SDK/world track of the overhaul. It carries forward Claude’s `hashed-hopping-lecun.md` plan, cited in the [data-enum work log](data-enums-work-log.md#2026-11-17--direction-changed-to-sdksworlds-phase-0-baseline-restored), and applies the subsequent decisions recorded on `main` at `d6ebc04`. The companion [compiler and runtime plan](compiler-runtime-overhaul-plan.md) owns AST, bytecode, enum representation, storage lifetimes, memory, and backend execution. Both tracks can advance against fixture SDKs and the verified interpreter contract before any public package migration or native cutover.

The first shared execution fixture is a bound call through an installed table in the verified interpreter. Use the [SDK registration terminology](sdk-registration.md#terminology) and the sequence in the [overhaul stages](overhaul-stages.md).

## What exists and what remains

| Foundation on `main` | Remaining work |
| --- | --- |
| `interface mod`, opaque records, worlds, `impl interface`, and checked world imports/includes/exports. | Generate `W::Env`, `W::Exports`, and `W::load`. Check world instantiation and actual exports. Verify identity and linkage across SDK majors. |
| Canonical interface/world digests, version records, and `#[since]` checks. The first handshake slice compares locked item uses with the candidate interface and accumulates typed differences. | Wire use-set comparison, dependency versions and same-version consistency into the loader. Settle the remaining digest compatibility questions before publishing a stable contract. |
| `Registry::install` checks bindings and produces an immutable `Installed` table. `install_for_program` links contracts to resolved declaration IDs. Verified scalar fixtures execute checked Cyallo references and synchronous host callbacks. | Extend installed-table execution to parking and collecting calls, declarative references, opaque contracts, and worlds. `NativeBindings` still supplies keys for default execution. |
| Intrinsic/member contract tables inform effects and GC metadata. | Resolve executable references, verify declarative operations, implement the safe emitter and conformance harness, and apply opaque contracts to ownership, transport, and tracing. |
| `std/prelude.ya` reexports types and variants. The embedded package is still the existing bundle. | Supply singleton `core`, convert the remaining packages to SDKs, and build the init world with the chosen authority grants. |

[SDK registration](sdk-registration.md) · [Pending SDK work](pending-decisions/05-sdk-worlds.md) · [Core decision](pending-decisions/01-core-module.md)

## Package, declaration, and runtime contracts

Supply one compiler-versioned `core` declaration package. It contains Option, Result, numeric constants and arithmetic, and struct utilities. Its availability grants no host authority. SDKs and worlds supply capabilities such as channels, clocks, tasks, and allocators through explicit environments. Parent components and Rust embedders implement the same checked world contracts. A prelude reexport preserves the declaring item's identity. Inclusion in an init world and implicit name visibility are separate decisions. [Provided environments](component-worlds/02-provided-environments.md) · [Core visibility and contents](pending-decisions/01-core-module.md)

Transparent types and methods come from ordinary Cyallo declarations. Allow host-independent bodies next to their declarations. A bodyless routine or opaque record binds through the owning runtime’s provider. Opaque types expose only validated contracts for construction, ownership, clone, destruction, tracing, erasure and transport. Transparent declaration identity is scoped by package, major, and declaration path. Opaque identities also carry the implementing instance where the world contract requires generativity. Resolve paths during linking and pass typed identities into checked bodies. Equal numeric IDs in independent runtimes denote independent objects. [SDK identity and versioning](component-worlds/05-sdk-coordination.md)

Connect the installed table to checked lowering and runtime dispatch. Check binding identity, declaration kind, instantiated signature and operand modes, digest, effects, locality, trace and cleanup requirements. Publish only after the full table validates. Registration and linking report missing, duplicate, stale, inconsistent, and wrong-kind bindings through typed errors. Expose immutable contract and dependency snapshots to compile workers. The process-wide pool supplies capacity. The runtime owns binding selection, feedback and publication and receives native candidates through its mailbox. Eliminate the private source-annotation compatibility path as each family migrates. [SDK registration](sdk-registration.md) · [Compile worker capacity](backend-design.md#compile-worker-capacity)

## Optimizable operations and the embedder interface

| Implementation form | How it runs |
| --- | --- |
| Cyallo body | Ordinary checked routine with inlining and specialization governed by its effects and ownership. |
| Declarative operations | Restricted, typed, bounded operation graph with checked projections and explicit ownership. The verifier checks effects, failure, and control flow before execution. |
| Guarded fast path | Validate type/layout/dependency guards before committing effects. The original checked body or declarative operation is the fallback and reuses the evaluated operands. |
| Host call | Dispatch across the typed runtime bridge with explicit effects, ownership and failure outcomes. Pending work holds rooted or owned state and records its continuation before submission. |
| Embedder intrinsic | Executable checked Cyallo or declarative reference plus optional backend-specific lowering through the safe emitter. Use the reference in the interpreter and whenever a native lowering is absent or its entry guard misses. |

Install engine-owned and embedder-defined intrinsics through the same contract validation path. Keep semantic primitive identities small and typed. Compile or resolve the reference under the signature and provider contract before admitting its lowered form. SDK methods such as enum predicates can then lower through normal calls, checked inlining and shape-based tag reads. The final buffer operation inventory and which collection operations have source bodies remain open. [Existing intrinsic contracts](intrinsic-contracts.md) · [Source-bodied methods](pending-decisions/04-source-bodied-methods.md)

The safe emitter accepts scoped typed operands, verified layouts and projections, a bounded operation builder, guards and explicit continuations. Shared preparation assigns runtime call ABI, roots, fuel, PCs, barriers and statepoints. Verify output against the contract before native publication. Raw code, guest pointers, arbitrary field offsets, custom calling conventions and direct root-map edits are outside the emitter interface. A verified structure constrains emission. Test executable semantics against the reference using controlled inputs, deterministic host fixtures and independent expected effects. [Compiler backend interface](compiler-runtime-overhaul-plan.md#native-preparation-backend-adapters-and-coroutine-recovery)

Supply a reusable conformance harness for interpreter, Winch and Cranelift. Preserve Claude's debug/test registration check as a target using registered conformance cases. Exercise eager/deferred checking, GC stress, guard miss, owned operands, failure, cancellation and suspension. Use interpreter single-stepping and native safepoint handoffs under the new backend contract. Compare values, observable effects, typed failures, fuel, roots and ownership transitions. Coverage from supplied cases supplements static verification and independent language tests.

## Load handshake and generated interfaces

Start with static typed instantiation for the init loader and fixture worlds. Reuse checked world declarations and import authority. A recipe records its declared world, SDK/component version requirements and optional lock data. Before checking and publishing a loaded component, validate the implemented world and SDK names, compatible version ranges, relevant used-item digests when lock data is present, and required exports. Without lock data, keep the world, version and export checks. Retain the typed distinction between world/version mismatch, same-version inconsistency, item mismatch and missing export. After the handshake, check the component against the world and its supplied environment. Publish only after successful initialization. [Handshake](component-worlds/05-sdk-coordination.md#load-handshake) · [Load-time checks](component-worlds/04-versioning-and-loading.md#load-time-checks)

| Generated declaration | Contract |
| --- | --- |
| `W::Env` | Record fields derive from resolved world imports, including includes and aliases. Reject collisions using world diagnostics. Each field exposes its declared interface. |
| `W::Exports` | Namespace type exposes the checked world exports, with optionality and version rules from the world contract. |
| `W::load` | Takes the recipe, typed environment, and supplies from the world design. Returns a typed result carrying `W::Exports` or the declared load error. The final SDK location of `LoadError` follows the package split. |

Use the same declarations to check host-provided modules and component exports. A runtime-supplied implementation may use guarded dispatch with provider/instance identity and version dependencies. The generic checked call remains available. Keep arbitrary recipe loading, catalogs, runtime environment construction, and revocation in the staged component-world work. [World declarations](component-worlds/03-worlds-and-sdk.md#instantiating-side) · [Component recipes and instances](component-worlds/06-components.md)

## Sequencing and decision gates

| Step | Deliverable and dependency |
| --- | --- |
| 1. Verify the starting point | Inventory SDK and provider consumers against `d6ebc04`. Preserve current language examples and record the 15 inherited failures separately. |
| 2. Install and consume contracts | Wire declarations and opaque types through `Installed` in a fixture SDK. Validate provider isolation, ownership, tracing and effects before migrating the embedded bundle. The compiler track supplies typed IDs, call descriptors and verifier inputs. |
| 3. Execute the reference path | The scalar fixtures resolve bound calls through the installed table and check results, typed failures and exactly-once host effects under budget pauses. Add declarative bodies and broader host contracts. Native conformance follows interpreter conformance and safe emission. |
| 4. Instantiate a fixture world | Implement the handshake and generated declarations using the existing resolver/digests. Verify version floors, use sets, missing exports, includes, aliases and narrowed environments. An adopted public digest contract depends on the digest decisions below. |
| 5. Supply `core` and migrate packages | Establish the agreed always-supplied declaration root. Apply user decisions before filling the full export inventory, choosing implicit visibility, splitting SDKs or enabling default capability grants. Update component-world and compiler docs for the `core` exception. |
| 6. Retire compatibility paths | Use declared nominal results and provider-bound opaque types across interpreter, native code, effects, constants, cache keys, REPL, GC and transport. Remove `StorageKind` and role special cases only when their consumers have migrated. Keep the enum shape optimizer and storage policy in the compiler track. |

Open decisions stay attached to their dependent work:

- **Packages and init.** Confirm the package names and split, init world grants, exact `core` contents and implicit visibility. Claude’s examples of package names, wildcard imports, and default grants are proposals. [Core record](pending-decisions/01-core-module.md) · [SDK record](pending-decisions/05-sdk-worlds.md)
- **Declarations and operations.** Confirm Range representation and typed source failure for `unwrap`/`or_fail`. Audit which generic traits, associated types, default methods and bounded buffer operations actual SDK bodies require before extending the language or the operation set. [Storage markers](pending-decisions/02-storagekind-removal.md) · [Source method failure](pending-decisions/04-source-bodied-methods.md)
- **Identity and compatibility.** Keep the existing checked digest implementation as the starting point. Before promising a stable format, resolve canonicalization/versioning, whether bodied items may be overridden, and treatment of bodies, contracts, references and lowering identity. The handshake compares declared use sets. Native publication separately checks the implementation dependencies compiled into the image. [Digest design](component-worlds/05-sdk-coordination.md) · [Current digest implementation](../crates/cyallo-ir/src/sdk.rs)
- **Performance and integration.** Record startup, dispatch, allocation, and native code measurements for representative enum, collection and task workloads. Profile the recorded Option/Result regression with comparable inputs. Apply the correctness and quality rules at cutover and follow the selected branch integration policy. [Performance record](pending-decisions/08-performance-and-checks.md) · [Repository record](pending-decisions/09-branches-and-repository.md)

The acceptance suite includes an independent user SDK, two runtimes with distinct bindings, moved/renamed declarations, opaque construction rejection, narrower environments, version/digest mismatches, ordinary `Result::Err`, dropped owned payloads and parked native calls. Use Cyallo CLI examples and independent expected outcomes. Verify compiler and runtime tests, all-feature checks, Clippy, format and vendor checks when compilation is authorized.
