# Data-oriented IR design for Cyallo

Cyallo has two routine paths during migration. Supported scalar source routines lower directly from indexed syntax to file-relative MIR. The linker sees their declaration headers, and semantic checking consumes MIR. Bodies outside that coverage still use linked AST checking. Both paths produce checked register IR for verified bytecode. [Routine pipeline](../crates/cyallo-ir/src/analysis/routine_pipeline.rs) · [Checked register IR](../crates/cyallo-ir/src/ir.rs) · [Verified interpreter baseline](startup-baseline.md#verified-interpreter-baseline)

## Startup goal and current pipeline

Apply the [readability and correctness rules](compiler-and-runtime-rules.md#readability-and-correctness) to every prototype. For example, derive a SIMD scan stride and its tail test lengths from the selected vector type’s lane count. Compare resulting byte offsets with the scalar scanner. Use the measured Rust record size for memory accounting. The proposed record size below is an experimental budget.

Optimize fresh-process time to first observable output for programs with many unused routines. Every body must still pass grammar validation before the first program effect. Keep verified bytecode and native preparation as the initial execution boundary. Reopening either contract requires an identified limitation, a measured cost where performance is the rationale, and a compatibility plan. The experiments below determine which changes ship.

Preserve each entry point's diagnostic timing. Declaration and import errors stay eager. Full checking reports an unresolved local reference even in an unused routine. Deferred compilation reports it when execution needs the affected routine. Retain one immutable parsed body per routine in the first prototype. Compare compact checked IR against the existing layout with allocation-free operand traversal before adopting it.

### Existing syntax conversions

Cyallo already indexes token ranges per parse unit. Its modern parser builds an owned syntax tree, then freezes that tree into indexed storage. Later the linker materializes owned trees again. Measure these conversions separately from lexing. Direct construction of indexed syntax could eliminate the first conversion, while letting name resolution consume indexed syntax could eliminate the second. Either can be evaluated independently. [Parse and freeze](../crates/cyallo-parser/src/units.rs) · [Linker materialization](../src/dynamic_modules.rs)

### Where startup work happens today

```text
Source → tokens → indexed AST
  supported routine → HIR → file-relative MIR → AIR → register IR
  other routine → materialized AST → linked AST → AST analysis → register IR
Register IR → verified bytecode → interpreter or native preparation
```

`lower_routine` still reads `cyallo_ast::Function` for local collection, type inference, ownership checking and register emission on the AST route. A migrated routine runs semantic analysis over MIR and uses its linked header for the contract. `ir::Function` contains typed registers, locals, branches in a flat instruction stream, storage placement and VM metadata. Verified lowering builds its CFG from that stream. [Compiler entry](../src/compiler.rs) · [Routine lowering](../crates/cyallo-ir/src/analysis.rs) · [Verified CFG](../src/bytecode/verified/cfg.rs)

| Boundary | Current work | Design pressure |
| --- | --- | --- |
| Source → syntax | Modern compilation parses units and freezes their syntax. Archived syntax materializes a mutable tree and shifts spans for linking. | Skipping balanced braces cannot preserve diagnostics for malformed expressions. Reusing immutable syntax could remove a copy without parsing twice. [Parser units](../crates/cyallo-parser/src/units.rs) · [Source materialization](../src/dynamic_modules.rs) |
| Syntax → linked declarations | Name resolution enters body scopes. Generic specialization and constant evaluation run before checked-module construction. | Moving these walks behind first call can change which programs perform effects before reporting errors. Constants and statically reachable constant callees require eager validation. [Namespace resolution](../crates/cyallo-ir/src/namespace.rs) · [Compiler entry](../src/compiler.rs) · [Architecture](architecture.md) |
| Linked declarations → deferred bodies | Module analysis lifts closures, computes cache dependencies where a cache is supplied, clones each deferred function and estimates its cost. Deferred analysis now borrows shared headers for local effect checking. The tracing scan also borrows bodies. | Measure the remaining function clone and body walks separately. Attribute work to the number and size of declared bodies and to the number of bodies actually forced. [Module analysis](../crates/cyallo-ir/src/analysis/modules.rs) · [Deferred analysis](../crates/cyallo-ir/src/analysis.rs) · [Cache keys](../crates/cyallo-ir/src/module_cache.rs) |
| Checked body → execution | Per-routine checked and bytecode cells defer translation. Verified lowering scans all checked bodies for global slots and trait implementations before it emits the reachable subset. | Use the baseline to verify demand and metadata discovery as the new pipeline takes over. A worklist fix changes which bodies the backend forces. [Routine storage](../crates/cyallo-ir/src/ir.rs) · [Verified lowering](../src/bytecode/verified/lower.rs) |

## Proposed compiler stages

```text
Source → lexer → token stream → parser → indexed AST
AST → desugar to structured HIR → lower control flow to MIR (per routine, file-local)
AST or MIR references → declaration and lexical resolution against the module graph
MIR + resolved bindings + declaration contracts → typed analysis and compile-time evaluation → AIR
AIR → type-directed specialization and emission → verified bytecode → interpreter or native preparation
```

| Product | Owner and representation | Completion condition |
| --- | --- | --- |
| Token stream | Per-file typed token kinds and UTF-8 byte spans into an immutable source revision. Benchmark bytes per second on representative Cyallo inputs and elapsed time to first output. | The scalar and optimized scanners agree on token boundaries and diagnostics, including at SIMD tails. |
| AST | Immutable indexed syntax partitioned by parse unit. Store ordered child lists as ranges, links as unit-local IDs and origins as file-relative spans. The source revision supplies spelling and trivia where required. | Parsing validates every body. A future versioned encoder can write deterministic sections with explicit tags, widths and offsets. Decoding validates ranges and source identity. Compression can then operate on those sections independently of Rust memory layout. |
| HIR | One structured routine using canonical calls, bindings, blocks, loops, matches and exits. Generated temporaries have fresh local identities. Each generated operation carries the originating source span. | Syntax-driven rewrites preserve evaluation count, order and scope. The same HIR can be built before imported contracts or operand types are known. |
| MIR | One routine body referencing the declaration index of its defining file. Record lexical scopes, local bindings, reference sites, import requests, evaluation order and source origins. Preserve qualified paths, member access and calls whose meaning depends on another declaration or inferred type. | Lowering accepts one indexed body, its file-local syntax context and fixed frontend options. A change to an imported implementation or export must yield identical MIR for the unchanged importer. |
| AIR | An immutable typed body for a particular declaration and instantiation. Analysis uses the module graph to bind declaration identities, establish types, effects, ownership, drop lifetimes and capabilities, and evaluate required constants under existing limits. | Execution forces the entry, required initializers, constant dependencies and subsequently demanded bodies. Full checking retains its existing coverage. An indirect call can retain a typed dynamic target. |
| Bytecode | Emission consumes specialized AIR and preserves logical PCs, source origins, roots and continuation metadata. | Verification precedes execution on the verified path. Native preparation and interpreter execution consume the existing contracts according to their supported coverage. |

**MIR construction is parallel by routine.** Workers read shared immutable file syntax and write independent routine bodies. Small routines may be batched into one job. A call records its callee expression, ordered arguments and source origin. Import lookup, overload selection, generic instantiation, effect inference and constant evaluation belong to analysis. Use the existing bounded compiler jobs and merge results in source order. Compare serial and parallel runs for identical MIR and diagnostics, and measure throughput across file sizes. [Parallel compilation](parallel-compilation.md)

### 1. Lex, parse and publish source syntax

Lex a file into typed tokens with half-open byte spans into its immutable UTF-8 source. In modern mode, the file shares its text, token buffer and line starts across parse units. The parser already returns a `ParsedFile` with unit-local `IndexedAst` tables. A `BodyHandle` retains the owning unit and identifies a function in that unit. An indexed node ID is meaningful within its owning unit. Diagnostics can pair the file identity with its relative span until the existing archive boundary translates it. [Parser snapshots](../crates/cyallo-parser/src/units.rs) · [Source positions](architecture.md#source-positions-and-bytecode)

Keep the existing grammar and unit boundaries for the first slice. Parse and validate all units before publishing a file, including unused routine bodies. On failure, select the same diagnostic in the same order under serial and parallel jobs. Time lexing, parsing, freezing and materializing syntax separately so the measurements show which conversion each experiment removes.

### 2. Desugar one routine into structured HIR

HIR stands for high-level intermediate representation. It makes syntax-driven rewrites explicit while keeping source-level structure. MIR is the middle representation that makes control flow explicit. AIR is the analyzed representation that carries resolved operations and their contracts.

The following are proposed modern-language contracts. HIR identifies an operator by its semantic enum variant. A dump may render addition as `opr::plus(a, b)`. That spelling belongs solely to the formatter. AIR resolves the operation from its identity, operand types and applicable implementation contracts. Explicit source calls still use ordinary declaration lookup. [Declaration rules](compiler-and-runtime-rules.md) · [Current range loop lowering](../crates/cyallo-ir/src/analysis.rs)

| Surface form | Structured HIR meaning | Verification case |
| --- | --- | --- |
| `a + b` | Apply the addition operator to the two operands in source order. AIR selects an implementation using their types. Short-circuit operators require conditional evaluation. | Each effectful operand runs once, in order. A numeric specialization preserves overflow behavior. |
| `for a in arr` | Evaluate `arr` once, convert it to an iterator once, then loop over calls to its advance operation. Match each result, bind the item for the body or break on exhaustion. | An empty input still performs conversion. Continue advances once, break skips the next advance, and all exits end the iterator scope. |
| `expr?` | Evaluate once and branch through the proposed Try protocol. Continue with its success value or convert the residual for the enclosing routine’s return type and return early. | The residual path skips subsequent expressions and runs normal scope cleanup. A returned error remains an ordinary result value. |
| Routine tail expression | Return the value of the body’s reachable tail expression. A nested block yields to its surrounding expression. Preserve the distinction between a value-producing tail and a terminated statement in the AST. | An explicit early return skips the tail. AIR checks the declared result on every reachable exit. |

HIR records scope and temporary boundaries, return targets, capture sites and origins. Its structural verifier checks local identities and valid exit targets. Generated operations inherit an origin from the syntax that caused them. Keep HIR transient unless a measured consumer needs to retain it. Compare peak storage and time through first invocation with direct AST-to-MIR lowering.

### 3. Lower one routine to file-local MIR

Build an ordered list of blocks with explicit terminators for branches, jumps and exits. Record local bindings, reference sites and the lexical scopes each edge leaves. Verify that every target belongs to the routine, that each block has a terminator, and that scope exits agree with source nesting. Store ordered call arguments and unresolved member receivers so later analysis can preserve evaluation order while choosing the operation.

For example, an iterator loop lowers to setup, advance, item and exhaustion paths. The body's `continue` targets the advance block. A `break` leaves the iterator scope and targets the continuation after the loop. MIR can describe those edges before the iterator type is known. Typed analysis selects the iterator operations and determines storage and cleanup obligations from the resolved contracts.

### 4. Resolve declarations and perform eager checks

Index declarations in deterministic module order, then resolve imports, exports and lexical references against the active module graph. Record bindings in a graph-owned overlay keyed by syntax position or reference-site ID. Keep references that require the body checker available for that check, including unresolved locals and members whose meaning depends on the receiver type. Preserve the first error selected by each entry point. Track the import and namespace facts a lookup consulted, including failed lookups whose diagnostics could change after an edit.

Freeze declaration facts that body jobs consume, including signatures, passing modes and conservative effect contracts. Keep compile-time constant initializers and their statically reachable callee checks on the eager schedule, under existing resource limits. [Declaration and constant timing](architecture.md#declarations-and-deferred-analysis)

### 5. Check and publish one AIR body per instantiation

Use the body and its resolved overlay to select declarations, instantiate generics, infer types and establish effects. When a standard declaration carries a validated intrinsic identity, carry it into AIR with the instantiated contract. An indirect call can retain a dynamic target under its checked callable contract. [Declaration rules](compiler-and-runtime-rules.md#builtins-follow-the-language-pipeline)

AIR analysis verifies loans and escaping references across returns, early exits and suspension using resolved passing modes, ownership, captures and control flow. It also owns drop-lifetime analysis. Use type and path-specific initialization and move facts to determine which owned values need cleanup on an exit and which must survive suspension. If one branch moves a local into the return value and the other leaves it initialized, only the latter path drops the local on exit. Check these obligations before publication.

Build the checked body privately. Resolve block fixups, validate the representation and publish the complete immutable body or its diagnostic. Entry bodies, required initializers and compile-time dependencies can demand analysis before execution. A runtime invocation can demand it later. Full checking forces all bodies and retains the coordinated transitive effect pass. [Deferred analysis](architecture.md#declarations-and-deferred-analysis)

### 6. Specialize and lower to executable code

Select operations using AIR's types, layouts, resolved declarations and ownership facts. For speculative specialization, test the guard before effects and preserve the checked fallback. Derive liveness, roots and continuation metadata against the fixed logical instruction order. Translate routine targets through executable linkage and preserve source origins, trapping behavior, polling and logical fuel.

The CLI defaults to the verified interpreter. The verified module can lower a selected routine and its transitive direct callees as a group. Track the bodies it forces separately from frontend body demand. The verified path validates executable layout before interpretation or native preparation. [Current engines](../README.md) · [Verified lowering](../src/bytecode/verified/lower.rs) · [Specialization rules](compiler-and-runtime-rules.md#specialization-uses-resolved-information)

## Module model and batched construction

Status: the linker implements files-as-modules over an explicit source list ([Namespace resolution](../crates/cyallo-ir/src/namespace.rs), `tests/file_modules.rs`). Every source beside a module that no `mod` line declares becomes a child module. `x.ya` and `x/mod.ya` name module `x`. A directory without `mod.ya` becomes an empty open namespace. A file module gates its children: they are private until it writes `pub use b;`, which the linker allows for a module's own child. `mod lib;` and inline `mod` keep working. The disk loader scans for a compiler entry: `ModuleLoader::load_component` (`cyallo-modules`) loads every other `.ya` file below a `.ya` entry's directory, in sorted order with the entry last. It skips hidden entries and any subdirectory holding `cyallo.toml` (a separate package), does not follow symlinked directories, rejects `.ya` leaf symlinks, stays inside the entry's read grant, and loads each file once. A `.yas` script and runtime imports (`load_parsed`) load declared files only. Two layouts are hard errors (E140): `d.ya` beside a directory `d/` of `.ya` sources, with or without a `mod` line, and an inline `mod b { ... }` beside a file `b.ya` or `b/mod.ya`. An unreferenced file is now a module, so a top-level statement in it is a module-initialization error.

| Cyallo | Rust analogue | Role in the pipeline |
| --- | --- | --- |
| Component | crate | Unit of interface, cache and invalidation. It has a root file, a name, the `.ya` files under its root and its dependency components. |
| File `a.ya` | module | Every file is a module named by its path from the component root. No `mod b;` line is needed. It is the unit of parsing, declaration indexing and parallel work. |
| Directory `d/` | module namespace | `d/b.ya` is module `d::b`. If `d/mod.ya` exists it is module `d` itself and acts as gatekeeper: its file children are private to `d` and its descendants unless `d/mod.ya` writes `pub use b;` or `pub use b::f;`. Without `mod.ya`, `d` is an open namespace and its children's `pub` items are reachable at `d::b::f`. |
| Inline `mod name { ... }` | inline `mod` | A manual namespace inside a file, such as `mod prelude { ... }`. Private unless written `pub mod`. It stays in the same file unit. A file `b.ya` and an inline `mod b` in one parent are a duplicate-name error. |
| `pub` | `pub` | The one visibility modifier, on items, `mod` and `use`. Without it a name is visible only inside its own module and child modules. |

- The directory form is `d/mod.ya` only. A `d.ya` beside `d/` is an error. `d::mod` is not a valid path.
- A symbol of `b.ya` is usable from `a.ya` if it is `pub` (or `a` is within `b`'s privacy scope) and named through `use` or a qualified path. Failed lookups are recorded as dependencies.
- Across components, importers consume the dependency's **interface**: signatures, passing modes, conservative effect contracts, types and constants of its `pub` items, plus a digest. They never read its bodies, except constant evaluation under existing limits. Re-exports keep the original declaration identity.
- Open: only `pub` and private exist today. A `pub(component)` level is not proposed unless needed.

### Entry modes: `.ya` modules and `.yas` scripts

The extension selects the mode, and neither mode falls back to the other (`src/main.rs`, `EntryMode` in `crates/cyallo-ir/src/options.rs`, `tests/cli.rs`):

- **`.ya` is a module file.** Its top level holds only declarations, `const` and immutable `let` (a `let` is a compile-time constant there). Mutable `let`, calls and other statements are errors in every `.ya` file. `cyallo run x.ya` requires the entry to declare `proc main()` and runs it. A `.ya` file with top-level statements is never run as a script.
- **`.yas` is a script.** Its top-level statements are the program. It never searches for or runs `main`. A script may use modules through ordinary `mod`, `use` and files-as-modules. A module may use only modules, never a script, because `.yas` files are not module candidates. Providing scripts through std, the SDK and world semantics is not implemented here. The component loading and authority proposal is in the [component worlds revision of 2026-11-23](component-worlds/README.md#revision-2026-11-23), which is a design only.
- Snippets (`compile(source)`) and the REPL use script behavior, with no extension involved. The startup generator in `scripts/startup_baseline.py` now writes `main.yas`.

### Batched IR construction

Four waves. Each is parallel inside and deterministic between. The bounded jobs from [Parallel compilation](parallel-compilation.md) execute them and results merge in source order.

1. **Syntax and MIR (per file).** The component's file set comes from listing its root directory, so there are no discovery waves by `mod` depth. Parse and validate every body, then lower each body to HIR and MIR. Pack bodies below a node threshold (`IndexedRoutineBody::estimate`) into one job. A large body gets its own job. MIR holds reference-site tables and no resolved targets, so it depends only on its own file.
2. **Declarations (coordination point).** Build the declaration index and resolve `use`, `pub` and `pub use`. Publish each component's interface and digest. Components run concurrently in dependency order.
3. **Overlays (per module).** With the index frozen, bind each reference site of each body into a graph-owned overlay, recording consulted lookups including failed ones. Select eager diagnostics in source order.
4. **Demand (AIR, then `ir::Function`).** Start from the entry, required initializers and constants. AIR needs only callee contracts, so a ready set is independent: batch by dependency level, publish each body once, then emit and hand the batch to verified lowering. Full checking forces every body through the same wave.

Batching units are routine-sized for storage and publication, module-sized for overlays and component-sized for interfaces.

## Data partitions and lifetimes

| Partition | Owner | Contents and lifetime |
| --- | --- | --- |
| Source and tokens | File revision shared by parse units | Source bytes, line starts, tokens and unit ranges. A unit or body handle keeps the source alive for diagnostics. |
| Indexed syntax | Parse unit | Typed syntax tables, ordered child ranges and body roots. Origins are file-relative and node IDs are unit-local. |
| Routine lowering | One routine builder, with immutable MIR where retained | HIR temporaries, block labels and fixups are private to construction. MIR scope and reference tables may be retained for eager checks while its operations are built on demand. |
| Resolution | Module graph snapshot | Declaration directory, import/export bindings, per-body overlays and lookup dependencies. Publish contracts before body jobs read them. |
| Checked body | Routine instantiation entry within a program snapshot | Immutable AIR and its typed side tables, or a cached diagnostic. Calls carry logical target identities. |
| Executable and native products | Existing executable and code publication owners | Linked function IDs, bytecode, logical PC mappings, roots and recovery metadata. Executing frames retain the code and metadata they need to resume. |

### Scheduling and failure publication

File parsing and syntax-only routine lowering can use independent jobs. Declaration indexing, import/export resolution, constant dependency ordering and transitive effect propagation introduce coordination points. Assign shared IDs deterministically and select diagnostics in source order when a wave must return one error. Batch small routines for scheduling while keeping the routine as the unit of storage and publication. [Parallel compilation](parallel-compilation.md)

For mutually recursive routines A and B, checking A can resolve B from the declaration directory while B is still deferred. Checking the call against the contract avoids waiting for a body in the same initialization chain. Constant evaluation follows its dependency and limit checks when it requires callee bodies. Concurrent demands for the same instantiation share its published result, including the original diagnostic on failure.

### AIR publication and dependencies

**AIR coverage can be partial across a program.** Publish a complete checked body per demanded routine instantiation. For a call from A to B, A can use B’s resolved signature and conservative effect contract while B’s body stays deferred. Taking a function reference alone need not force B. First invocation forces B before its body can perform effects. Required compile-time evaluation can force B earlier. Concurrent requests share the same body result or diagnostic. The bytecode emitter consumes only published bodies. V8 uses first invocation to trigger compilation of eligible lazy functions. Cyallo applies that scheduling idea after eager parsing and name checks. [V8 lazy parsing](https://v8.dev/blog/preparser) · [V8 lazy compilation entry](https://chromium.googlesource.com/v8/v8/+/a98309795399032afd2f191adaa94ed6e9918437/src/runtime/runtime-compiler.cc#228)

These reuse cases assume that all other inputs stay stable. Track dependencies where a consumer reads them, including contract changes derived from a body edit.

| Change | Reuse and invalidation |
| --- | --- |
| Caller source revision changes | Build fresh source products for the edited input and rebuild its affected overlays and checked products. |
| An imported export or alias changes | Reuse unchanged importer syntax and file-local MIR. Recompute affected lookups and invalidate checked results or diagnostics that depended on them. |
| A callee signature or consumed effect contract changes | Reuse unchanged caller syntax and MIR. Recheck affected instantiations against the new contract and regenerate their executable products. |
| A callee body changes with its consumed contracts intact | Rebuild the callee's checked and executable products. The ordinary runtime call in the caller's AIR remains reusable. Recompute compile-time results that read the body and invalidate optimized code that used the previous body. |
| A frontend or analysis option changes | Invalidate products from the earliest stage that consumes the option and any products that depend on them. |

The startup risk is retaining a full AST and a second full body representation for every unused routine. Compare file-local MIR against direct resolution over indexed AST. Measure peak simultaneous storage, eager reference-index construction and first-call materialization separately. A candidate split emits the MIR declaration, scope and reference tables eagerly and materializes its operation stream per body on demand. The eager tables must support the agreed diagnostics on their own. [Indexed AST](../crates/cyallo-ast/src/indexed.rs) · [Current module compilation](../src/compiler.rs) · [Backend contract](backend-design.md)

## Migration from the current body pipeline

| Slice | Implementation work | Evidence to proceed |
| --- | --- | --- |
| 1. Run a routine through new IR | Add structured HIR and explicit-block MIR for a scalar routine. Check that MIR and publish a typed body. Emit the existing `ir::Function` from the typed body through a compatibility adapter. | A normal `cyallo run` call executes a routine processed by the new stages. Tests demonstrate that analysis and emission consume their declared inputs. |
| 2. Build file-local MIR from indexed syntax | Move the migrated syntax-to-MIR path ahead of the mutable linker. Take an indexed body handle and file-local context. Resolve graph-dependent references through a graph-owned overlay. | The same unchanged importer builds identical MIR across edits to an imported signature or body. The relevant bindings, AIR or constant results are rebuilt. |
| 3. Migrate semantic consumers | Add direct and indirect calls, captures, generics, constants, and ownership-sensitive operations in slices. Move each check and rewrite to the stage that owns its inputs. | Check diagnostic and behavior parity per feature before removing the previous routine path for that feature. Publish only complete typed bodies. |
| 4. Route the backend through typed bodies | Make verified lowering consume the checked representation and discover global slots, referenced routines and used implementations from demanded bodies. Preserve extra roots and persistent-session state. | Verify direct, indirect and trait calls, globals and continuations. Rerun the zero-routine, unused and call-all profiles and report bodies checked and verified images emitted. |
| 5. Remove transition copies and measure layout | Delete the redundant resolved-body copies as their consumers move. Compare allocation-free traversal over the transitional representation with compact storage. | Attribute improvements to the changed stage. Compare first output, first invocation, full checking and resident bytes. |

### First executable slice

The modern compiler selects supported root source functions while it still has indexed syntax. This covers by-value `i64` parameters and an `i64` result, parameter reads, integer and boolean literals, grouping, arithmetic, comparisons, `if` and `return`. The verified interpreter consumes the resulting register IR through its normal entry point. [Routine pipeline](../crates/cyallo-ir/src/analysis/routine_pipeline.rs) · [Verified interpreter entry](../src/vm.rs)

| Stage | Input and output for this slice |
| --- | --- |
| Structured lowering | A parse-unit body handle is the input. HIR records operator kinds, parameter identities, expression origins, and the structure of the `if` and returns. |
| Control-flow lowering | HIR produces MIR with routine-local IDs, ordered operations, an entry block, and explicit branch and return terminators. Validate block targets and local references when the builder finishes. |
| Semantic analysis | MIR and the routine's declaration contract produce typed blocks, typed operation identities and source origins. Check both reachable exits and preserve the current arithmetic and diagnostic behavior. Read expression and control-flow data from MIR. |
| Register emission | Translate the typed blocks to the current `ir::Function` and its logical instruction locations. Reuse existing storage, liveness, GC and resume finalization. The current verified bytecode emitter consumes the result. |

The compiler omits selected bodies when it materializes the mutable linker input. After linking, the module graph associates file-relative MIR with the linked routine symbol. Its body fingerprint participates in the module-cache key. Constants and deferred analysis use the retained MIR, and source archive offsets are applied when emitting instructions or reporting diagnostics. The existing route handles unsupported forms during migration. A semantic diagnostic from the new route is the routine's result. Graph-dependent reference resolution and the remaining routine syntax still need to move to the new pipeline. [Source preparation](../src/dynamic_modules.rs) · [Module cache](../crates/cyallo-ir/src/module_cache.rs)

Run [the scalar branch fixture](../examples/routine-ir.yas) with `cyallo run examples/routine-ir.yas`. It prints `24` and `13` on successive lines. A build with `--features startup-profile` reports one `bodies_prepared_from_indexed_syntax` and one `bodies_emitted_from_air` at first output when run with `CYALLO_STARTUP_PROFILE=json`.

### Slice 2 status: direct calls

HIR and MIR now carry direct calls to `i64` functions. MIR records each call as a reference site (path text, span, argument count) plus ordered operand ranges, and stores no resolved target. The MIR digest of an importer therefore cannot depend on a callee declaration. A unit test checks this by changing a callee's signature and body without changing the importer's text length.

Resolution is now direct over indexed syntax for this subset. The linker (`bind_indexed` in `namespace.rs`) takes each indexed body's reference sites, resolves them against the module namespace and checks the callee's linked contract (plain by-value `i64`, matching arity, not a local, intrinsic or unit variant). If every site binds, the retained product is the MIR plus a per-site overlay of callee symbols, and no owned tree is ever materialized. If any site does not bind, the body is thawed from indexed syntax (`BodyHandle::materialize_body`) and linked as a tree, so the existing resolver resolves or reports it with its usual diagnostics and timing. The overlay is part of the body digest. Measured effect on scalar workloads is in [startup-baseline.md](startup-baseline.md#scalar-workloads-through-the-new-pipeline).

### Slice 3 status: locals, assignment and loops

The migrated subset adds `let`/`let mut` (optionally annotated `i64` or `bool`), assignment and compound assignment to mutable locals, `while`, and `for i in a..b` or `a..=b` over `i64`. HIR resolves names with block scoping and a flat local table (parameters first, then locals and hidden loop locals). MIR gives loops explicit header, body, test and back-edge blocks, and keeps a value inside the block that defines it, so loop state lives in locals. A `for` is lowered with an emptiness test before the bound is adjusted, so an exclusive range cannot overflow on an empty range. AIR defines each local at its first store, checks annotations and later stores against the local's type with the existing diagnostics, and allows back edges only for jumps. Reads and stores keep the existing `Address`/`Load`/`Store` form so storage placement, liveness and resume metadata are reused. Measurements are in [startup-baseline.md](startup-baseline.md#locals-assignment-and-loops-through-the-pipeline).

### Performance work during migration

The [verified-interpreter baseline](startup-baseline.md#verified-interpreter-baseline) gives the comparison point for demand and metadata costs. Reuse the harness for the zero-routine, 4,096-unused-routine and call-all cases. Keep the default deferred CLI separate from explicit eager `--engine ng`. Scope the standalone worklist optimization as an independent execution scheduling change if it lands before the pipeline migration. It can reduce forced body checks while the compiler still uses a linked AST as its analysis input. [Benchmark harness](../scripts/startup_baseline.py) · [CLI dispatch](../src/main.rs)

Cache-key work and the deferred-body clone remain measurable during the transition. Keep warm module reuse separate from fresh-process results. Key binding overlays by typed syntax IDs scoped to the owning unit and body. Use spans for source origins and diagnostics. Compare eager and deferred behavior on malformed syntax, unresolved references and constant callees throughout migration. [Module cache](../crates/cyallo-ir/src/module_cache.rs) · [Name resolution tests](../tests/resolution_bindings.rs) · [Constant dependency tests](../tests/module_units.rs)

## Candidate checked representation

### Identity and ownership

Both MIR and AIR are per routine. A file index owns declaration headers and identifies each MIR body. A program snapshot owns a directory of routine instantiations, with a known contract available before a body is published. MIR calls can refer to a local name or path through a routine-local reference-site index. AIR direct calls identify a directory entry with a typed logical ID. Linking translates the target to the executable’s existing function ID. This supports recursion and calls to bodies whose storage has yet to be allocated. The directory entry owns or shares each published body. [Checked routine ownership](../crates/cyallo-ir/src/ir.rs) · [Verified direct calls](../src/bytecode/verified.rs)

**Compact references.** Prototype a per-routine table of direct callees only if call-site density justifies the extra lookup. A call operand then indexes that table, whose entries hold full target IDs. Select a narrow or wide encoding once when freezing each body, using checked representability of the largest actual index. For byte offsets, check the largest referenced byte displacement in its own arena. A small caller can target a distant callee, so its instruction count cannot establish that an inter-body displacement fits. Keep physical addresses out of the serialized representation. True base-relative pointer compression requires the owner to guarantee reachability and address stability throughout use. Compare it separately if pointer-heavy resident storage appears in profiles. [V8 pointer compression](https://v8.dev/blog/pointer-compression)

Measure against direct function IDs, including table storage, alignment, decode cost and peak memory. Narrowing an operand only saves space if the enclosing layout shrinks. Test the largest representable index, the first index requiring the wide form, recursive calls and first invocation of a deferred target.

Use routine-local typed 32-bit IDs for registers, instructions, blocks and operand-list offsets. Keep mutable register semantics, including repeated definitions and addressable cells. Register identities describe storage that may be written more than once. Check conversions and offset-plus-length at construction. Report overflow through a concrete error variant carrying the routine and table kind.

### Per-body storage

| Table | Contents | Main access pattern |
| --- | --- | --- |
| Instructions | Ordered typed opcode records. Common fixed-arity fields can be inline. | Sequential emission and inspection |
| Call sites and operands | Call descriptors and ranges into an ordered register array. | Decode one site and visit its arguments in order, including repeats. |
| Typed payloads | Variable or uncommon data such as instantiated contracts, constants and ownership-sensitive projections. | Decode on demand from the instruction or descriptor. |
| Blocks and origins | Instruction boundaries and source origins. Start with one span per instruction. | Resolve branches and recover source locations without reading unrelated payloads. |
| Derived metadata | Liveness, root and continuation data for the frozen instruction order. Temporary analysis indexes belong to their consumers. | Read at lowering, safepoints and resume boundaries. |

Prototype a dense array of compact Rust enum records for the hot instructions. A binary record carries an operator, destination and two source IDs inline. A direct call carries a call-site ID indexing a descriptor with resolved callee, optional destination and a range in a shared register operand array. Store exceptional payloads in typed pools, including instantiated contracts, constant values and ownership-sensitive projections. Store origins in a parallel array, initially one span per checked instruction for exact diagnostic parity. Reserve split tag/payload arrays as a measured challenger for tag-only passes. The proposed inline target is 16 bytes per hot record, subject to an actual layout probe. Pools, spans, padding and reserved capacity count toward total memory.

At Cyallo revision `699d04addf361e9a0daa4c62eaa3c92caac83653`, a standalone Rust layout probe against the workspace crate on aarch64-apple-darwin measured `size_of::<ir::Instruction>() = 88`, `Op = 80`, and `Reg = 8` bytes with rustc 1.101.0-nightly (07e526db9). These are inline sizes. A million initialized instruction slots alone occupy 88 MB before owned argument lists. The compact target gives no prediction of application startup time. [Checked instruction definitions](../crates/cyallo-ir/src/ir.rs)

Expose typed decoded views and an allocation-free operand visitor. Preserve operand order, multiplicity, optional destinations and the separate address owner. For example, subtracting a register from itself visits two input occurrences. Build reverse edges only for a consumer that needs them, scoped to the analysis. Give the builder symbolic block labels, resolve them to instruction boundaries at freeze, and discard fixups. Share CFG boundaries with downstream consumers where their splitting rules agree. Source-statement markers, trapping instructions, polling points and suspension boundaries retain their logical identities even if native layout changes. [Verified CFG](../src/bytecode/verified/cfg.rs) · [Runtime rules](compiler-and-runtime-rules.md)

## Experiments and rejection criteria

- Establish a release-build baseline with a fixed entry that prints once, and vary unused routine count, body size and imported module count independently. Measure process launch to first output through the same output sink across variants. Also time first call, full check, warm module reuse and hot execution. Record host, revision, jobs, filesystem-cache state, median and p95 over randomized paired runs. Fresh process and cold filesystem cache are separate conditions.
- Count parsed, copied, resolved, checked and translated bodies. Record allocated bytes by stage, peak live bytes, operand arity histogram and time in header cloning. Compare eager runs at one and multiple jobs. Use a program that eventually calls every routine to expose work shifted past first output.
- Compare the existing representation, the existing representation with allocation-free use traversal, and compact storage with the same traversal. Include construction, linking, freeze, liveness and lowering. Test arithmetic-heavy, call-heavy and metadata-heavy bodies. Reject compact storage as a startup priority if simpler changes explain the gain or if temporary coexistence of old and new bodies dominates peak memory.
- Differentially run eager and deferred modes over syntax errors in unused routines, alias/shadowing, recursive generics, closure captures, constant dependencies, failed guards, GC stress and suspension with a one-instruction budget. Compare diagnostic payloads and spans, observed effects, roots, continuation PC and logical fuel. Exercise existing interpreter and native paths within their supported coverage. Keep semantic and performance rollout decisions independent.

### Approved experiment order and acceptance criteria

Measure parser freezing and linker materialization separately. The historical 4,096-routine case spent 11 ms in freezing and 9 ms in materialization, compared with 61 ms in name resolution before declaration indexing. Recheck those proportions after the current baseline refresh and compare reductions in whole-pipeline allocations as well as time. [Startup baseline](startup-baseline.md)

Use a fresh process with an empty persistent compiler cache for the primary startup measurement. Report warm reuse separately. Measure elapsed time from launch through completion of the first called routine alongside time to first output. A first-call regression beyond measured run-to-run noise requires an explicit tradeoff decision before adoption.

Use the following comparisons to attribute changes to the right stage.

| Workload | Isolates | Record |
| --- | --- | --- |
| Many unused routines with a fixed entry | Eager cost per declaration and per parsed body byte | First output, syntax copies, overlay construction, deferred-body allocations |
| The same program calling one large routine after the first output | Work deferred until demand | First-call completion, body-analysis time and bodies forced by backend preparation |
| A program that eventually calls every routine | Total compile work moved across the first-output boundary | End-to-end time, peak live bytes and cumulative body/bytecode time |
| An importer rebuilt after a signature edit or body-only edit | Dependency granularity | Reused and rebuilt overlays, checked bodies and executable products |
| Serial and parallel full checks | Scheduling overhead and deterministic publication | Full-check time, peak memory, selected diagnostic and source span |

Use nightly Rust `std::simd` for explicit vectorization. The proposed lexer experiment compares scanning for the next LF in a `//` comment with scanning horizontal whitespace, each against its scalar baseline. Require identical token spans and diagnostics, and measure the effect on end-to-end startup. [SIMD lexer research and benchmark plan](simd-lexer-research.md)

## What three compilers store

| Compiler and IR | Instruction identity and layout | Consequence for a pass |
| --- | --- | --- |
| Zig AIR | Instruction index into separate tag and payload arrays. Two operand references fit in the eight-byte payload. | A tag scan can skip every payload. Decoding a variable-length operation can require another array access. |
| V8 Turboshaft | Byte offset into a growable buffer of variable-size operations. Input references follow each operation's fixed fields. | A visitor can read an operation and its operands together. Iterating operation boundaries also uses a size table. |
| OpenJDK HotSpot C2 | Arena-allocated nodes with pointer edges and a dense numeric index for phase data. | A rewrite can visit the users of a value directly. Mutating an input requires maintaining the reverse edges. |

### Zig ZIR and AIR

The Zig checkout at `5e36170b5ff1cb7710aa9b1c5540b7d11aa62537` uses `MultiArrayList(Inst)` in both ZIR and AIR. ZIR stores source string bytes separately and puts variable payloads in a word array named `extra`. AIR's instruction tag selects the interpretation of its payload. An addition carries two references directly. A typed instruction with a larger payload can carry a type and a payload index. The opcode's contract determines which extra fields follow. The eight-byte figure describes the payload alone. [ZIR storage and payload formats](https://codeberg.org/ziglang/zig/src/commit/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/lib/std/zig/Zir.zig) · [AIR instruction definitions](https://codeberg.org/ziglang/zig/src/commit/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/src/Air.zig)

### What a semantic instruction produces

Mitchell Hashimoto’s 2022 walkthrough traces an addition through Zig semantic analysis. Known operands can produce a typed compile-time result. Runtime operands can require an AIR instruction. The local checkout preserves this distinction with a different representation from the article’s per-AIR values array. An `Air.Inst.Ref` can identify an interned value or an AIR instruction. Sema maps ZIR instructions to analyzed references. [Sema walkthrough](https://mitchellh.com/zig/sema) · [AIR reference conversions](https://codeberg.org/ziglang/zig/src/commit/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/src/Air.zig#L1234) · [Semantic analysis](https://codeberg.org/ziglang/zig/src/commit/5e36170b5ff1cb7710aa9b1c5540b7d11aa62537/src/Sema.zig)

For Cyallo, this suggests a narrow experiment inside deferred body lowering. When the existing constant evaluator has already proved a value, carry its typed constant reference until a consumer requires a register. Materialize it there using the existing copy and rooting rules. Keep the expression’s source origin available independently of register allocation. Compare emitted constant loads and peak register count on the same body. Verify diagnostics, logical fuel and owned aggregate copies against current lowering before adopting the change. This experiment can use the approved indexed syntax and resolved-name tables directly. [Constant evaluation](../src/constants.rs) · [Checked IR](../crates/cyallo-ir/src/ir.rs) · [Constant execution contract](architecture.md)

### V8 Turboshaft

The Bazel source snapshot identifies itself as V8 15.8.30. Its owning module pins `a98309795399032afd2f191adaa94ed6e9918437`. Turboshaft allocates operations in eight-byte slots, reserving at least two slots per operation. The size depends on the concrete operation and its input count. Graph growth copies the buffer. Retained offsets continue to identify operations after relocation, while a borrowed address must be reacquired. The buffer records sizes at both ends of each allocation for forward and backward traversal. Replacing a loop phi in place requires the new operation to fit the old allocation. [Graph storage and replacement](https://chromium.googlesource.com/v8/v8/+/a98309795399032afd2f191adaa94ed6e9918437/src/compiler/turboshaft/graph.h) · [Operation size and trailing inputs](https://chromium.googlesource.com/v8/v8/+/a98309795399032afd2f191adaa94ed6e9918437/src/compiler/turboshaft/operations.h) · [Operation index](https://chromium.googlesource.com/v8/v8/+/a98309795399032afd2f191adaa94ed6e9918437/src/compiler/turboshaft/index.h)

### Java HotSpot C2

In OpenJDK `jdk-21+35`, C2 assigns each node an index for phase-specific side arrays. A node's required inputs are ordered, so two occurrences of one value in a subtraction occupy distinct slots. Reverse edges let a pass enumerate uses. Replacing one required input removes one reverse edge from the old definition and adds one to the new definition. C2 also distinguishes required inputs from scheduling precedence edges. These choices let a local rewrite update dependent nodes while imposing maintenance costs on every affected edge. [HotSpot C2 Node and set_req](https://raw.githubusercontent.com/openjdk/jdk/jdk-21%2B35/src/hotspot/share/opto/node.hpp)

### Implications for Cyallo

These designs serve different access patterns. A pass that counts operation kinds can scan a tag column. A lowering pass that visits every operand can benefit from contiguous operand storage. A worklist that repeatedly asks which instructions use a definition benefits from reverse edges. For Cyallo, measure scans, argument traversal, and rewrite frequency independently. Include construction time, side tables, and operand storage when comparing memory or throughput. The reference layouts alone establish no speedup for Cyallo.

### Token classification in Pierre

Pierre’s JavaScript highlighter groups lexical properties in a byte per token kind. Its bracket stack uses one bit for the bracket kind and seven for ternary depth, saturating the latter at 127. For Cyallo, static classifications keyed by a typed token kind are a candidate if profiles show repeated classification cost. Grammar and scope depth must remain exact through the supported nesting range. Boundary tests should compare diagnostic spans and errors. [Pierre JavaScript scanner](https://raw.githubusercontent.com/pierrecomputer/pierre/main/packages/highlights/src/langs/js.wat)
