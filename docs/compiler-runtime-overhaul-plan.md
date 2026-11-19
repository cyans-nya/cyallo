# Cyallo compiler and runtime overhaul

The replacement pairs an immutable frontend and verified register bytecode with an interpreter, Winch, and Cranelift. The interpreter uses frame storage. Native code uses registers and stack objects for eligible values. All tiers share the language contracts for ownership, enum layout, calls, allocation, and recovery. The target platforms are macOS and Linux on AArch64 and x86-64.

This is the compiler and execution track. The companion SDK/world plan carries forward Claude’s design and owns package migration, provider installation, intrinsic registration, and world instantiation. The numbered pending records remain the source of truth for unresolved user choices. Build and validate the replacement in implementation slices, then switch execution to it in one cutover. The language and runtime contracts supply the expected behavior. Executable verification awaits a build. [SDK and worlds plan](sdk-worlds-overhaul-plan.md) · [Pending decisions](pending-decisions/README.md) · [Compiler and runtime rules](compiler-and-runtime-rules.md)

The shared implementation sequence and stage exit gates are in the [overhaul stages](overhaul-stages.md).

## Starting point and open decisions

`main` carries the data-enum work at `d6ebc04` and the backend design, SDK terminology, and pending records at `8e1cbef`. `overhaul` carries the language server, editor integrations, and module graph/linking at `62aa665`. The branches share parent `f51c909`. [Repository record](pending-decisions/09-branches-and-repository.md)

| Open decision | Dependent work | Work that can proceed |
| --- | --- | --- |
| Exact `core` exports beyond Option, Result, numeric constants/arithmetic and struct utilities. Placement of GeneratorStep and send/receive errors. | Final export inventory and package placement. Host capabilities stay in SDKs/worlds either way. | Create the singleton core identity and resolve the agreed declarations. [01](pending-decisions/01-core-module.md) |
| Explicit imports, implicit `core` scope, and visibility of bare variants. | Name resolution, migration guidance, and user-facing examples. | Build package injection and resolve qualified declarations. [01](pending-decisions/01-core-module.md) |
| `Range` as a declared struct or opaque provider type. | Remove the Range marker and select its declaration contract. | Migrate other marker producers independently. [02](pending-decisions/02-storagekind-removal.md) |
| Inline enum shapes, size/alignment thresholds, and niche filling. | Select optimized representations and tune them. | Design descriptors and verify an explicit-discriminant reference layout. [03](pending-decisions/03-layout-optimizer.md) |
| Typed failure and source-span semantics for Cyallo-bodied `unwrap` and `or_fail`. | Rewrite those methods and retire their intrinsics. | Lift E173 for supported inherent methods and migrate predicates or other helpers with settled semantics. [04](pending-decisions/04-source-bodied-methods.md) |
| SDK package names and split, and default capabilities in the init/prelude world. | Move public declarations and create the default environment. | Implement registry integration, intrinsic checks, the handshake, and world generation against fixture SDKs. [05](pending-decisions/05-sdk-worlds.md) |
| Physical scope reclamation via explicit release operations or runtime scope tracking, and the proof used for payload-borrow invalidation. | Enable storage reuse and remove `parked_children`. | Specify logical lifetime events, loan obligations, and tests that either implementation must satisfy. [06](pending-decisions/06-memory-lifetimes.md) |
| Lazy constructor function emission, identity-based tests, or both. | Choose the remediation for the recorded failing tests. | Reproduce and classify failures by name. [07](pending-decisions/07-baseline-failures.md) |
| Repository-wide formatting or keeping the existing formatting baseline. | Any broad formatting change. | Run a read-only format check and distinguish inherited differences. [08](pending-decisions/08-performance-and-checks.md) |
| Merge direction and target, whether to push `main`, and ignore rules for local worktrees/logs. | Merge, remote updates, and ignore-file edits respectively. | Compare overlapping changes and document the tests needed on the integrated tree. [09](pending-decisions/09-branches-and-repository.md) |

The historical startup figure for the CLI is about 8 ms. Check the cost of always supplying `core` with comparable cold and warm measurements. Pre-resolve and cache its declarations by compiler and package version. Treat the historical figure as baseline evidence until measured again. [Core record](pending-decisions/01-core-module.md)

## Compiler modules and immutable syntax

Keep syntax, checked CFG, canonical bytecode, native preparation, backend emission, and execution as separate modules with one-way dependencies. The parser publishes source syntax. The semantic frontend resolves declarations, types, traits, effects, ownership, and provider contracts. Shared lowering elaborates storage, cleanup and calls. The bytecode module owns its codec, verifier and disassembler. Backend adapters consume verified native input without parser, AST or resolver dependencies. The execution layer owns scheduling, runtime memory, publication and code retirement.

Modern source and REPL compilation publish indexed `ParsedFile` snapshots. In-memory module graphs publish snapshots in file order. File discovery reads module declarations directly from the indexed tables and passes the same snapshots to the CLI, `compile_path`, and runtime loads. Modern compile plans retain the static graph as file-relative snapshots, with archive offsets and synthetic runtime packages stored separately. Callers with an existing snapshot can use `compile_snapshot` or `compile_snapshot_for_execution`.

Compatibility compilation uses the shared indexed representation before checking and preserves its case-folded identifiers and source spellings. Both parsers still build temporary source trees before freezing. The Modern linker and Compatibility checker materialize mutable trees at their input boundaries. Core package parsing, direct table construction, and moving generic and deferred checking onto indexed syntax remain open. Measure time and retained memory for the combined parse, freeze, and semantic materialization path before calling the frontend migration complete.

Use distinct typed IDs for syntax, declarations, registers, blocks, layouts, word offsets, call sites, and code versions. Give reusable runtime handles generations. Use semantic enums for outcomes and typed errors with payloads. Derive offsets, sizes and alignment from layouts. Keep encoded discriminants in the codec or ABI and policy constants in named configuration. Give syntax, bytecode and runtime ABI one authoritative definition each. Isolate unsafe code in audited modules for memory/ABI access, executable memory and stack transitions. Run verifiers in release builds. Keep each optimization in a named pass with preconditions, dumps, a disable switch and checks of preserved invariants.

Use immutable snapshots and indexed arenas built with ordinary Rust vectors. Freeze each parse unit into owned slices. A file shares the token buffer across units, and a deferred `BodyHandle` holds an `Arc<ParsedUnit>` and a `BodyId`. Retaining one body retains its unit and the source/token snapshot. The remaining units can be released independently.

| Interface | Representation and rules |
| --- | --- |
| `SourceSnapshot`, `TokenBuffer` | Owner-qualified source ID, digest, `Arc<str>`, line offsets and one token array. Parse cursors borrow ranges and synthesize EOF. Spans use checked file-relative byte offsets. |
| `AstBuilder`, `ParsedUnit` | Worker-owned tables and reusable scratch lists. Validate ranges, roots, child IDs and list bounds before publication. Assemble in source order. A recovered tree for tooling cannot enter normal lowering without successful validation. |
| `ExprNode`, `ExprKind` | A span and typed syntax variant. `Binary(BinaryExprId)` refers to `{operator, left: ExprId, right: ExprId}`. `Call(CallExprId)` refers to `{callee, type_arguments: ListId<TypeExprId>, arguments: ListId<ArgumentId>}`. Larger alternatives use typed payload tables. |
| Declarations, patterns, statements and type expressions | Separate typed tables. A function stores syntax for parameters, generics, attributes, return type, captures and body. Types remain unresolved. Names refer to per-unit interned spelling with occurrence spans. Compare contents after hashing. |
| Typed views and visitors | Borrow the owning unit and expose typed child views. Generate traversal, printing and structural checks from the syntax schema. Bound parser nesting and use iterative walks where depth is untrusted. |
| Semantic products | Store resolved types, call targets, ownership and captures separately. A generic instance holds a `BodyHandle` plus substitutions. Deferred analysis shares syntax and publishes checked results. Cache by snapshot/body identity, substitutions, dependency versions, options and provider contracts. |

Preserve syntax checks on unused bodies and errors in both dialects. Closure conversion creates semantic functions with source origins. Delete recursive syntax cloning in generic instantiation and deferred scheduling. Use bump allocation for scratch only if profiling justifies the change and resource cleanup is explicit. Keep the initial snapshots uncompressed and rebuild a file's units when the source changes.

| Source examined | Design influence |
| --- | --- |
| Oxc AST/parser 0.155.0 and allocator/string crates 0.157.0 | Arena-backed concrete payloads, source-borrowed text and generated visitors. Cyallo freezes independently owned tables to permit shared deferred work. [Oxc AST source](https://github.com/oxc-project/oxc/tree/main/crates/oxc_ast) |
| CPython 3.14.0 | PEG actions build trees described by ASDL. Its arena manages raw storage and separately tracked resources. Use a shared schema and keep resource ownership explicit. [CPython ASDL](https://github.com/python/cpython/blob/v3.14.0/Parser/Python.asdl) · [PyArena](https://github.com/python/cpython/blob/v3.14.0/Python/pyarena.c) |
| V8 15.8.30.0 | Zone-owned parse data and lightweight preparsing motivate separate temporary and retained data. Preserve Cyallo's full syntax checks in this migration. The inspected V8 compression concerns snapshots, while Chromium Blink manages parkable strings. [V8 preparsing](https://v8.dev/blog/preparser) · [Blink parkable strings](https://chromium.googlesource.com/chromium/src/+/HEAD/third_party/blink/renderer/platform/bindings/parkable_string.cc) |
| Zig `5e36170b` | Indexed AST tables, reused scratch storage and typed views inform the frozen AST. Separate lowering keeps semantic changes out of syntax. [Zig AST](https://github.com/ziglang/zig/blob/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/lib/std/zig/Ast.zig) |

## Core, enum layout and provider bindings

Supply one logical versioned `core` declaration package per program. It is always supplied and is the single exception to the component-worlds “no single std” rule. Core availability does not determine implicit visibility. Its agreed contents are Option, Result, numeric constants/arithmetic and struct utilities. Keep channels, clocks, tasks, and allocators in host SDKs/worlds. Revise the component-worlds and compiler/runtime documents to distinguish supplied core declarations from capabilities granted by an environment. [Core decision](pending-decisions/01-core-module.md) · [Component worlds](component-worlds/README.md)

Option, Result, GeneratorStep, SendError and ReceiveError are ordinary nominal enums. The declared variants are the authority. Use `Option::Some`, `Option::None`, `Result::Ok` and `Result::Err` and keep the helper constructor functions removed. Runtime producers instantiate the selected declaration's return type and obtain a validated variant mapping from its operation contract. Structural validation checks the expected payload types and arities while retaining nominal identity. Resolve provider names at link/install time. Lowering, runtime and native code use validated declaration and operation IDs.

Replace `Type::Storage` producers in lookup, intrinsic results, generators, allocation and concurrency with instantiated declared results. Remove the compatibility rules in region comparison and generic unification once all callers migrate. Migrate Range, ParseError, AllocError, LoadError, ViewError and TypeDescriptor to opaque provider types or declared data types according to their contracts. Range remains gated. Remove redundant constructor intrinsics, reserved semantic variant IDs, `EnumEncoding` roles, and `Value::NativeEnum`/`NativeEnumStorage` only after constants, matching, GC, interop and native execution use ordinary variants. Type syntax may still express language-level `gc`/`rc` and locality. Standard opaque types come from declarations and the per-runtime registry. [Marker inventory](pending-decisions/02-storagekind-removal.md)

The generic layout module takes instantiated variant shapes, child layouts, target size/alignment constraints and approved representation rules. It returns a `LayoutId` with discriminant encoding, typed projections, initialization, move, trace and destruction information. Equal layout inputs must select the same representation regardless of declaration name or SDK. Nominal type identity remains distinct. Use a reference explicit-discriminant implementation for conformance. The inline eligibility and niche policy are open. Remove `is_core` when all consumers use the common descriptor. [Layout decision](pending-decisions/03-layout-optimizer.md)

Enable source-bodied inherent enum and struct methods through the ordinary checked routine pipeline. Cover receiver modes, generics, effect checks, moves from receiver fields, imports and diagnostics. Migrate host-independent predicates and mapping helpers before deleting their member intrinsics. Keep failure-producing methods behind their source failure and span decision. [Source-bodied methods](pending-decisions/04-source-bodied-methods.md)

## Verified bytecode and scope storage

The checked CFG carries resolved type, operator, call, effect and ownership identities. Elaborate moves, loans, cleanup and storage scopes before encoding the internal versioned register bytecode. A function contains typed locals and temporaries, block parameters, place projections, instruction/source identities, constants, argument lists, call sites, initialization paths and statepoint metadata. Use a schema-defined `u32` word stream with an opcode and instruction word count in the header. Store variable-length operands in typed pools. The verifier checks decoding, block edges, type and ownership joins, projection facts and continuation metadata.

Operators resolve as trait-function calls. Use primitive arithmetic only after resolving a trusted primitive contract or proving the checked body implements it. Lowering must preserve user overloads, aliases, imports, shadowing and evaluation order.

References in the catalog identify design influences. **M** is [Rust MIR](https://rustc-dev-guide.rust-lang.org/mir/index.html), **D** is [drop elaboration](https://rustc-dev-guide.rust-lang.org/mir/drop-elaboration.html), **J** is [JVM 27 instructions](https://docs.oracle.com/en/java/javase/27/docs/specs/jvms/jvms-6.html), **V** is [V8 Ignition](https://v8.dev/docs/ignition) and [feedback-directed calls](https://v8.dev/blog/wasm-speculative-optimizations), and **C** is Cyallo's own contract.

| Instruction family | Example | Contract | Ref. |
| --- | --- | --- | --- |
| `storage.live`, `storage.dead` | `storage.live local` | Start uninitialized storage and end it after cleanup and loan obligations. | M, D |
| `const`, `const.materialize` | `r = const k` | Load an immutable descriptor or explicitly materialize an owned value. | J, C |
| `value.copy`, `value.move`, `value.clone` | `r = value.move p` | Require a copy contract, transfer initialized ownership, or execute a checked clone plan. | M, C |
| `place.init`, `place.replace` | `old = place.replace p, move new` | Initialize empty storage or replace through a checked operation with explicit old-value ownership. | M, C |
| `aggregate.init`, `enum.init`, `enum.tag` | `enum.init p, variant, payload` | Track partial initialization and publish the active variant once initialization succeeds. Tag reads allocate no payload. | M, D, C |
| `borrow.shared`, `borrow.mut`, `loan.end` | `r = borrow.mut p, loan` | Acquire and end checked access. Compute the effective projection once. | M |
| `check.bounds`, `check.variant` | `check.bounds i, len` | Establish a fact required for a projection if the CFG has not already proved it. | M, J |
| `int.checked.*`, `int.bit.*`, `int.shift.*` | `r = int.checked.add a, b` | Apply the selected width, signedness, overflow and shift semantics. | J, C |
| `float.*`, `compare.*`, `convert.*` | `r = compare.eq a, b` | Use language-defined floating-point, comparison and conversion contracts. | J, C |
| `jump`, `branch`, `switch` | `jump block(args)` | Explicit typed control flow and ownership transfers for block arguments. | M, J |
| `environment.init`, `owned.alloc_init`, `closure.bind` | `r = closure.bind @code, env` | Initialize captures in an existing place, explicitly allocate an owned environment if needed, or bind code to an environment without allocation. | C |
| `call.direct`, `call.indirect`, `call.closure`, `call.virtual` | `call.closure @site, c, args` | Resolved, thin callable, environment-aware or trait-mediated call with typed arguments and outcomes. | M, J, V, C |
| `call.runtime` | `call.runtime @site, args` | Invoke a validated runtime operation through its typed ABI. | C |
| `gc.alloc_init`, `rc.alloc_init`, `rc.init_in` | `r = gc.alloc_init @desc, payload` | Initialize traced or counted storage. Custom RC uses checked backing and allocator state. | C |
| `rc.clone`, `rc.downgrade`, `rc.upgrade`, `rc.*_count` | `r = rc.upgrade weak` | Explicit strong/weak ownership operations and observable counts. | C |
| `backing.alloc`, `backing.release` | `backing.release move token` | Create and consume an opaque move-only backing token. | C |
| `drop.place`, `drop.value` | `drop.place p` | Execute only active destruction obligations and retain the phase if cleanup parks. | D, C |
| `return`, `failure.propagate` | `return operand` | Transfer result or propagate typed runtime failure after cleanup. A returned `Err` is an ordinary result. | M, J, C |
| `poll`, `generator.yield` | `poll @statepoint` | Fuel, cancellation and scheduling handoff, or transfer to a generator's resumer. | C |
| `trap`, `unreachable` | `trap reason` | Typed trap or verified unreachable control flow. | J, C |

Places cover locals, globals, fields, indices, dereferences and variant payloads. Fallible, collecting, reentering and parking operations specify outcome edges and the state published at each transition. Separate the phase before an ownership transfer from the phase after it. Only success initializes the result destination unless the operation explicitly returns a language-level error value.

Emit logical lifetime events for trivial values as well as values with destructors. Track initialized, partially initialized, moved and dropping paths. End loans at their proven last-use boundaries. Preserve specified destructor timing and assignment order. Elaborate cleanup for loop iterations, break, continue, return, failure and cancellation. Abrupt stop releases runtime resources under its separate contract. Frame capacity may remain reserved, while initialized values and scope-owned resources end independently. Native register promotion and stack slot reuse require the same lifetime and address-exposure facts.

The experimental scalar format charges fuel per encoded instruction, including storage markers and cleanup-edge jumps. Scalar lowering derives storage events from value liveness and keeps a borrowed scalar home live through the last alias use. Normal return transfers its result and requires every other slot to be dead. Failure discards the scalar frame. General ownership cleanup and equivalent cross-tier fuel accounting still need their broader execution contracts.

Verified bytecode v7 lowers checked IR through typed blocks, carries non-executable statement metadata, and resolves executable PCs after lifetime and edge cleanup insertion. Statement markers retain incoming edge identity where several paths share a PC. Shared and exclusive scalar loans retain logical homes across borrowed calls. Call sites distinguish direct routine targets from installed host targets. [No-op removal](backend-design.md#remove-executable-no-ops-before-interpreter-conformance) · [SDK bindings](sdk-registration.md#using-it)

The scalar verifier derives an interpreter frame layout after checking logical storage transitions. It encloses each register’s reachable live ranges in a single instruction interval and shares a slot only across disjoint intervals. Block boundaries account for storage carried across edges, including backward jumps. The layout can conservatively retain extra slots when an interval spans a gap. The word stream and its source map retain logical register and instruction IDs. Native stack placement needs a target-specific layout and address-exposure analysis.

`cyallo bytecode examples/functions.psc --verified 1` selects linked routine 1 and disassembles its verified scalar image, including lifetime instructions, direct call targets, frame-slot assignments, and source spans. This inspection command uses the existing source entry point. It does not select a new runtime engine.

The current non-GC mitigation parks child indices by parent. Frame release makes raw indices reusable without retiring the corresponding cache entries. Source paths also need checks for aggregate reinitialization and the owner of newly materialized payload addresses. Verify stale parent/child identities, nested payload replacement and projections, and rejection while a payload loan is live. An iteration's plain storage must become reusable even when the frame never exits. The physical release mechanism and loan proof remain open. Remove `parked_children` only once their replacement satisfies these invariants. [Current cell management](../src/runtime.rs#L833) · [Lifetime decision](pending-decisions/06-memory-lifetimes.md)

## Closures and managed memory

Represent a callable by its function instance, signature, environment layout and capture plan. Record routine kind (`fn` or `proc`), effects, parameter and result types, and environment access separately. A procedure can return a value. A function's effect restrictions still allow relevant failure, allocation and fuel obligations.

| Environment access | Invocation contract |
| --- | --- |
| Shared | Ordinary repeatable calls borrow the environment. Non-Copy captures stay owned by the closure. |
| Exclusive | Repeatable calls requiring mutation acquire exclusive access for the duration of the call, including suspension. |
| Consuming | A checked one-shot capability transfers ownership of the environment at the call commit point. It permits moving owned captures out and destroys remaining captures on cleanup. |

Keep ordinary call syntax for repeatable values. Express consuming access through declaration-backed callable capabilities for functions and procedures. Their public placement depends on the open core/SDK inventory. Carry required access through generics, erasure, trait dispatch and the ABI. A consuming-only closure cannot satisfy a repeatable callable contract.

Use an empty environment for capture-free callables, frame or stack storage when the environment is proven nonescaping, and an owned allocation when an owned environment escapes. Borrowed environments stay within their referents' lifetimes. Captures use explicit copy, move and borrow operations. Invocation borrows or transfers the actual environment and does not clone every capture. An explicit structural clone follows the capture plan. Native calls use a hidden environment operand and typed projections. An inline cache may prove the code target, while each invocation still supplies its own environment. [Current closure execution](../src/runtime.rs#L1441)

Bytecode specifies the allocation or ownership operation, its effects, and its success, failure and cleanup transitions. The runtime owns physical allocation, quotas, backing validation, collection, reference counts, weak state and cleanup scheduling. Backends lower through the common ABI. A managed allocation descriptor carries payload layout, independent trace and destruction plans, storage kind, locality, allocator binding and failure behavior. `@shared` remains a typed rejection pending its runtime contract.

- A GC handle copy preserves identity and adds a reference where live. A local lifetime end removes that root. Collection reclaims unreachable objects, including pure GC cycles.
- An RC move transfers an owner without changing counts. Explicit cloning creates an owner. Last strong release commits before payload destruction. Weak upgrade fails once destruction has started. Keep allocator state and control-block identity available to parked cleanup and to remaining weak owners where required. Strong RC cycles remain uncollected. This migration does not promise reclamation of cycles through strong RC ownership.
- Trace live GC references inside RC payloads, initialized aggregates and closure environments. Visit only the active initialized variant. Relocation transfers the representation without cloning, releasing counts or running the old copy's destructor.
- Evaluate allocation operands once. Root inputs while acquiring and validating backing. Keep initialization state until the new owner commits. On failure, release every acquired resource exactly once. A fallible allocator operation with a declared `Result` returns the declared enum error. Runtime allocation failures follow their typed failure edge.
- Invoke custom allocation and deallocation through resolved procedure calls with explicit suspend/reentry states. Queue collector-discovered cleanup as rooted work and run guest cleanup outside tracing and relocation. Shared memory lowering owns barrier selection. Publish valid object state before reentrant destruction wherever the operation requires replacement before cleanup.
- In the initial replacement, explicit managed allocations retain observable identity, counts, allocator effects, quotas and failures. [Managed memory](managed-memory.md) · [Concurrency contract](concurrency.md)

## SDKs, worlds and the embedder seam

The two tracks share one interface for bound calls. The SDK linker produces validated declaration and operation identities, instantiated signatures, opaque lifecycle contracts, and installed binding dependencies. Checked lowering records passing modes, effects, cleanup, and continuations in call descriptors. The verified interpreter executes the reference or registered runtime operation. Native preparation consumes the same descriptors to produce validated ABI and statepoint plans. Exercise the fixture SDK through its installed table in the verified interpreter before native emission and the public package migration. [Detailed SDK plan](sdk-worlds-overhaul-plan.md) · [SDK terminology](sdk-registration.md#terminology)

Finish the path from SDK declaration to installed binding. Opaque declarations and bodyless routines and methods obtain contracts from the owning runtime's registry. The provider's `NativeBindings`, interface digests, worlds and contract tables are existing foundations. The installed table and executable reference semantics need execution integration. Resolve signatures and operand modes at installation. Validate matching item identity, digest, kind, ownership, locality, effects, tracing and safepoint obligations before admitting a binding. Cache compiled dependencies against the validated contract and required provider version. The process-wide compile pool supplies capacity. The replacement schedules instance-owned jobs with queue and in-flight caps and round-robin service. Each runtime receives results through its mailbox and owns publication, registry and feedback. [SDK registration](sdk-registration.md) · [Compile worker capacity](backend-design.md#compile-worker-capacity)

| Work | Deliverable and acceptance |
| --- | --- |
| Implementation forms | Cyallo bodies run as checked routines. Declarative operations lower through verified IR. Guarded fast paths retain the checked fallback. Host calls declare effects and use the host bridge. Embedder intrinsics supply executable reference semantics and may supply native lowerings. |
| Reference harness | Resolve each intrinsic reference to a checked Cyallo body or typed declarative implementation. Use it for interpreter execution and fallback. Test native lowerings against expected behavior and the reference, including results, effects, operand ownership, failures, guard misses and statepoint cases. |
| Safe emitter | Give embedder lowerings typed operands, checked projections, a bounded emission context, permitted operations, and explicit guards and exits. Verify output against the installed contract before backend admission. Use shared native preparation for layouts, runtime bridges, barriers and statepoints. |
| Load handshake | Validate declared SDK/world names, versions, relevant used-item digests and required exports before publication. Preserve the documented lock-data branch and typed mismatch errors. Follow world and environment authority checks before instantiation. |
| Generated world declarations | Derive `W::Env`, `W::Exports` and `W::load` from the checked world, including aliases, includes and export contracts. Generate ordinary declarations. Exercise mismatches and verify that failed loads do not publish partially initialized instances. |
| Init/prelude and packages | Replace the re-export-only prelude with the chosen init world and capability bundle. Migrate std declarations into the chosen SDK packages. These public changes wait for the package split and default grants. Update the world documents for the settled `core` exception independently. |

[SDK implementation backlog](pending-decisions/05-sdk-worlds.md) · [Generated world interfaces](component-worlds/03-worlds-and-sdk.md#instantiating-side) · [Load handshake](component-worlds/05-sdk-coordination.md#load-handshake)

## Native preparation, backend adapters and coroutine recovery

The [detailed backend design](backend-design.md) specifies the proposed crate interfaces, ABI, publication and recovery obligations, and the direct cutover tests. Before native adapter work, execute the interpreter's independent conformance fixtures for resumable execution, lifetimes and the bound call.

Canonical bytecode stays immutable. Native preparation consumes verified bytecode and an immutable feedback snapshot. The runtime stores feedback by function instance and semantic call-site ID. Record target, layout or shape, provider/body version, counts and empty, mono-, poly- or megamorphic state. Use bounded storage with an initial named polymorphic capacity of four. This is an initial tuning default from the earlier draft. Snapshot data is advisory and cannot establish authority or ownership.

Evaluate the callable, receiver and arguments once. Guard target, layout and version before the call's ownership or effect commit. On an entry miss, dispatch the checked generic call with the same evaluated operands. A later guard exits to a recovery state for the current logical frame and commit phase. Revalidate dependencies at installation and pin code versions while active or suspended. [V8 speculative inlining and deoptimization](https://v8.dev/blog/wasm-speculative-optimizations)

Use one shared inliner for static calls, eligible eager operator inlining, and feedback-directed calls. Preserve current inlining hints and keep budgets in named policy. Carry over the initial `Auto`, `Preferred`, and `Always` budget tuples from the earlier draft. In order, those are `(32, 64, 256, 3)`, `(64, 128, 512, 3)`, and `(256, 256, 1024, 8)` for callee instructions, expanded site cost, caller growth, and depth. `Never` disables inlining. Treat these as tuning defaults. Remap locals, blocks, loans, drop paths, storage scopes, environment operands and call sites. Preserve failure and parking continuations, logical frames, source origins, fuel and accounting. Recheck the result. Inlining follows deferred-body policy and records typed decision reasons when it cannot proceed.

Expose the backend compile seam as `compile(CompileRequest<'_>) -> Result<UnlinkedCodeImage, CompileError>`, alongside a descriptor for target and supported features. Input is a `VerifiedNativeFunction`, immutable target/layout/ABI environment, and compile policy. The shared producer owns semantic legalization, memory lowering, ABI preparation and recovery. Winch and Cranelift adapters own emission and machine representation. [Zig AIR](https://github.com/ziglang/zig/blob/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/src/Air.zig) · [Zig code generation](https://github.com/ziglang/zig/blob/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/src/codegen.zig)

| Backend seam | Contract |
| --- | --- |
| Scalars and flow | Emit selected typed scalar operations, checked outcomes, blocks, block arguments and branches. Shared preparation defines language failure edges. |
| Frame and memory | Request stack objects with layout, alignment, lifetime, purpose, address exposure and relocation classification. Emit verified projections and accesses with supplied alignment, alias and barrier facts. |
| Calls | Emit a prepared guest call or system-stack bridge. The plan supplies `FnAbi`, operand and result locations, ownership commit, statepoint, failure/park outcomes and continuation. |
| Statepoints and recovery | Bind symbolic roots, relocatable references and logical values to recorded machine locations. Emit verified import/export plans for entry, handoff, OSR and deoptimization. |
| Finalization | Resolve final frames, labels, locations, code sections and typed relocations. Produce normal and OSR entries, source/inline maps, the four state maps, ABI/target versions and code dependencies. |
| Publication | Validate maps, versions, dependencies and relocations. Apply executable memory permissions and instruction-cache synchronization. Install executable images and retire them when no activation retains them. |

Reject incomplete images before entry. When the native tier cannot compile an operation that the verified bytecode interpreter supports, select that interpreter at entry and record the typed reason. Native code uses typed runtime calls and verified handoffs. A general per-instruction VM execution helper would violate the adapter contract.

Retain Go-style copying coroutine stacks and defined native safepoints. Guest frames and audited transition code use the movable stack. Opaque Rust and host calls use a worker system stack. Check frame requirements before callee entry. Stop the coroutine exclusively before copying, repair every mapped stack reference and the saved CPU context, then reload and resume. Preserve evaluated arguments and their ownership across growth. Off-stack state stores owned values or activation-relative handles. [Go stack growth](https://go.dev/src/runtime/stack.go) · [Go scheduler and system stacks](https://go.dev/src/runtime/HACKING)

| Map | Required contents |
| --- | --- |
| GC roots | Live traced references and derived-reference reconstruction. |
| Stack relocation | All live references into the moving stack, including non-GC borrows, frame links and saved context. Moving statepoints publish writable locations. |
| Ownership and cleanup | Initialized paths, moves, owners, drop obligations and cleanup phases. |
| Recovery | Logical and inlined frames, immutable code versions, continuation phases, and reconstruction recipes. |

For loop OSR, import the active top frame at a verified loop entry. Keep addressable homes when outstanding borrows need them. Prepare destination storage before transferring ownership and select recovery recipes by commit phase. Deoptimization reconstructs logical frames without replaying effects. Arbitrary-instruction debugger stepping uses interpretation. Native activations hand off at defined statepoints.

Keep scheduler identity, fuel and processor leases explicit. Park a waiting coroutine without its processor lease. Register operation identity, destination and continuation before host submission. Preserve lexical generator yield to its resumer and keep it distinct from coroutine parking. Update the all-PC fallback language in the concurrency docs after verifying the new execution contract. [Concurrency runtime](concurrency.md) · [Language progress tracker](concurrency-language-progress.md)

## Verification, performance and migration

Specify expected results, effects, diagnostics and resource bounds from the language, runtime and SDK contracts. Encode them as independent Cyallo fixtures. Exercise eager/deferred analysis, the verified interpreter, Winch and Cranelift, with GC stress and native safepoint handoffs where relevant. Keep source examples in `.ya` files. The verified interpreter becomes the executable reference for native tiers after it passes the independent fixtures. Agreement among tiers supplements those assertions. Legacy outputs, failures and timings can help locate historical changes. Resolve any undocumented behavior against a contract decision before turning it into a test expectation.

| Area | Acceptance cases |
| --- | --- |
| Frontend | Both dialects, malformed and deeply nested syntax, deterministic parallel assembly, stale/cross-unit IDs, retained deferred bodies and substitution-specific diagnostics. Measure removal of token-fragment copies, individual node allocations and recursive syntax clones. |
| Declarations and layouts | Singleton core identity under the selected visibility policy, aliases and shadowing, overloads, independent provider instances, equal layouts for equal shapes, nominal type distinction, malformed variant mappings, source-bodied receiver methods. |
| Storage and memory | Bounded storage in long-running no-GC loops, nested aggregates, enum overwrite and projection, stale cell reuse, live-borrow rejection, partial moves, early exits and cleanup. Stress allocation failure, GC during initialization and weak upgrade while finalizers park. |
| Closures and calls | Repeatable shared/exclusive calls, consuming and cloning behavior, escaping and invalid borrowed captures, erasure, distinct environments at one call site, inlining with cleanup and checked arithmetic. |
| World and runtime | Invalid bindings, typed load mismatches, authority checks, reference/native conformance, parked allocator calls, exactly-once host effects, cancellation and abrupt stop. |
| Native verification | Malformed encoding, ownership joins, incomplete maps, rejected relocations, guarded fallback, stale versions, copied stacks with live borrows, thread migration, loop OSR and deoptimization. |

Reproduce and classify all 15 recorded failures against the relevant revisions and configurations. At least some tests refer to fixed function indices, including the attribute, tiering and stateless procedure cases. Check the failure of each before attributing all 15 to enum constructors. Recover contract-level assertions about layout, tiering and accounting from those tests. Record representation-dependent expectations and unresolved behavior separately. [Recorded failures](pending-decisions/07-baseline-failures.md) · [Example indexed test](../tests/attributes.rs#L20)

The Option/Result loop showed a reported slowdown of roughly 16–18% against the recorded baseline. The programs and constructor paths differ, and there is no profile. Re-run equivalent workloads under comparable conditions and report interpreter, Winch and Cranelift results separately. Profile enum construction and matching, frame setup, type clones, allocation and dispatch before tuning. Measure lexing, parsing, traversal, resolution, generic and deferred work, and single-worker versus parallel throughput. Record allocation count, bytes, peak and retained memory, native compilation, code size and execution. Verify the recorded release sizes of `Type` (48), `Value` (64), `Op` (80) and `Instruction` (88) bytes while they remain in use, then report compiled sizes and total retained memory for the replacement. Benchmark claims require whole-frontend or end-to-end evidence. [Performance record](pending-decisions/08-performance-and-checks.md)

Once compilation is authorized, run workspace/all-feature tests, targeted language and release checks, Clippy, vendor checks and `cargo fmt --check`. Record the `engines`, `gc` and `objects` benchmark results separately with the environment. Correctness, structure, and auditability govern acceptance. The handoff reports 241 existing format differences. Apply broad formatting only under the chosen policy. [Code quality rules](backend-design.md#code-quality-rules)

| Sequence | Exit condition |
| --- | --- |
| Establish the baseline | Record revisions, configurations, conformance cases and comparable profiling inputs. Keep inherited failures visible. |
| Separate syntax from semantics | Immutable snapshots, token slicing, declaration resolution and generic/deferred analysis preserve behavior. |
| Introduce checked execution | Verified bytecode and reference interpretation cover storage and cleanup. Migrate declarations, layouts, closure environments and RC/GC as their decisions permit. |
| Finish provider and world contracts | Installed bindings, reference harness, emitter and handshake pass their tests. Migrate package paths and init grants after their decisions. |
| Add native execution | Shared preparation, Winch and Cranelift produce verifiable images. Exercise retained stacks, relocation, safepoint handoffs, and loop OSR on the target matrix. |
| Reconcile branches and cut over | Follow the selected merge direction and timing. Preserve editor/module integration and the specified data-enum semantics. Require independent correctness and the backend quality and structure rules for acceptance. Record benchmark results with the environment. |

### Direct execution cutover

1. Keep the replacement independently runnable during development. A missing operation returns a typed unsupported result before execution. A verifier failure on compiler-generated bytecode is a compiler error. These outcomes make coverage and lowering defects visible during bring-up.
2. Exercise the full source-to-execution path on the integrated candidate. Inventory normal execution, REPL, debugger, constant evaluation, dynamic load and calls, and bound calls. Verify that each required path emits or consumes verified code and assert its observable behavior. Close any required coverage gap before proceeding. Test the native tier matrix and record performance on workloads that meet the same semantic assertions.
3. Switch the runtime entry points in the same integration change. Route all Cyallo execution through verified bytecode and the new interpreter or native adapters. Interpreter selection, guard recovery, and deoptimization stay inside that pipeline. Remove the legacy execution route from the shipping dispatch and check that disabled native tiers, unsupported native operations, and debugger stepping all select the verified interpreter.

Per-routine admission still decides which **new** tier executes a routine. The migration itself is a single switch of execution pipelines. Branch integration and public SDK package migration retain their separate decisions.

Branch direction, remote publication, and ignore rules remain separate user decisions. [Branch and workspace record](pending-decisions/09-branches-and-repository.md)
