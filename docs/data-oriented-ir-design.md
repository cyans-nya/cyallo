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

### Slice 4 status: break, continue, short-circuit, unary, bool

The subset adds `break` and `continue` (inside a loop; elsewhere the existing route reports E115), `&&` and `||`, unary `-` and `!`, and `bool` parameters, results and callee contracts. A loop keeps a frame of pending jumps that are finished once its exit and continue blocks exist; a `for` gives its counter test its own block so `continue` can land on it. A short-circuit operator keeps its result in a hidden local, because the right side runs in its own block and values do not cross blocks. For the same reason it is lowered only where no earlier sibling value is waiting: as the root of a condition, initializer, assigned or returned value, or nested through `&&`, `||`, parentheses and unary operators, never under another binary operator or as a call argument. Such a routine stays on the existing route.

One deliberate difference from the existing route: it never checked that the operands of `&&` and `||` are boolean (it moved the operand into a boolean register), although its unreachable second path shows the intended `mismatched types` check. The pipeline reports that error (E109 on the whole expression), so `n && true` with an `i64` `n` is now rejected on the migrated route. The first attempt declined such routines instead, but a body the linker has already dropped cannot be handed back to the existing route, so a decision that depends on types must be a diagnostic, not a fallback.

### Slice 5 status: procedures; numeric types deferred

`proc` routines with by-value `i64` or `bool` parameters now lower through the pipeline: bare `return`, calls to other procedures as statements, no missing-return check, and `Op::Call` without a destination. The linker's contract check pairs a statement call with a procedure callee and a value call with a function callee; any other pairing (a function called as a statement, a procedure used as a value) stays on the existing route. Call arguments are reported at the argument's span, as the existing route does. A procedure can only reach `print`, references and globals through forms outside the subset, so most real procedures still decline; observable parity for the ones that qualify is limited to what the subset can express.

Sized integers and floats were deferred at this slice and are covered by the next one.

### Slice 6 status: tree-walking AIR and numeric types

MIR no longer flattens expressions. A block holds statements whose expressions are trees (`mir.rs`), joined by explicit exits, and AIR walks those trees (`air.rs`). The earlier descriptions of MIR as a stream of typed-less operations in slices 3 and 4 are superseded; the control-flow structure they describe is unchanged.

The existing route types numbers in two interacting passes, and AIR now reproduces both in the same order. `infer` types an expression upward from its operands, including the numeric context that `numeric_context` picks from them. `contextualize` pushes an expected type down into the literals of an arithmetic tree (`let y: i32 = x + 1` retypes the `1`), where an integer literal in a float context is left alone and fails later, a float literal in an integer context is an error at the literal, and a literal that does not fit is a conversion error at the literal. A plain integer or real literal keeps its contextual form (`Literal::Integer`, `Literal::Real`) until a context types it, as in the existing route. Expected types come from annotations, a local's type, a callee's parameter types and the routine's result. Because AIR types as it walks, it also builds the blocks: `&&` and `||` end a block in the middle of an expression, so a MIR block may become several, and their count is known syntactically before any is built.

Before lowering, the existing route fixes every local's type: an annotated local has its annotation, an unannotated one takes the first type that its initializer or a later assignment can be given, a loop counter takes the type of its range, and the first local still unknown reports why. AIR repeats that fixpoint (`infer_locals`), so a typing error there comes before any lowering error, and lowering reads the fixed types. A binary operation also infers both operands with errors propagating before it lowers either, as the existing route does.

The subset now accepts `i64`, `bool`, `f64` and the sized integers (`i8` to `u64`, `isize`, `usize`) for parameters, results, annotations and callee contracts. The `RealDivide` operator is still left to the existing route, as is anything outside the forms listed in the earlier slices.

Verification is differential. `tests/routine_ir_parity.rs` includes a deterministic generator of random numeric functions (six types, literals including negative and fractional ones, arithmetic and comparisons, annotated and unannotated locals, assignments and compound assignments, calls). Each program compiles twice, once on the migrated route and once with an inert out-of-subset statement that keeps it on the existing route, and the two must agree on whether it compiles, on the diagnostic (code, message and the source text of the span) and, when it runs, on how it ends and what it prints. Five seeds of 4,000 programs each (20,000 programs) agreed; the committed test runs 400, and `ROUTINE_FUZZ_CASES` and `ROUTINE_FUZZ_SEED` widen the search. The fuzzer found three ordering differences that handwritten tests had missed, all fixed: the inference order above, the operand typing before each binary operation, and the explanatory hint on an integer-to-float mismatch.

Clone removal: MIR takes ownership of HIR, because HIR is transient, so expression trees move instead of being copied. `contextualize` returns a borrow when no literal changes, which includes an `i64` literal in an `i64` context, so the common case allocates nothing. Calls no longer copy the callee's parameter list. Paired against the build before this change (15 rounds, two passes each), call-all at 4,096 routines went from about 251 ms to 236 ms to first output, and unused routines improved by about 3%. Behavior is unchanged: the 13 parity tests pass, and 4,000 further generated programs on a new seed agree. Typing is still done twice per expression (`infer`, then lowering), which is the next candidate.

Cost: walking trees and typing twice (infer, then lower) makes AIR heavier. Unused routines are unaffected because AIR runs only on demand; the call-all workload at 4,096 routines went from about 223 ms to about 249 ms to first output.

### Slice 7 status: lazy control-flow graph and a text digest

Profiling a prepared body at 4,096 routines showed the cost was not where the plan assumed. Building HIR took about 5.5 ms, lowering it to MIR about 2.4 ms, and the SHA-256 digest of the MIR tree about 18 ms, because hashing a tree feeds the hash thousands of tiny pieces. Two changes follow. The body's digest is now one hash of its own source text, which is still independent of every imported declaration, so an unchanged importer keeps its key when an import changes (a comment edit now changes the key, which is a harmless over-invalidation). And HIR waits in the retained body until typed analysis first needs the control-flow graph, which a routine that is never run never does. What stays eager is what the linker and admission need: the reference sites, an eligibility decision (HIR construction), and node and loop counts taken from HIR.

Paired against the build before this change (15 rounds, two passes each), first output for unused routines went from about 82.6 ms to 66.2 ms at 4,096 and from about 318 ms to 253 ms at 16,384, and call-all at 4,096 from about 236 ms to 221 ms. The remaining time at 4,096 unused is front-end work (lex, parse, freeze, header materialization) and HIR construction.

### Slice 8 status: each binary node is typed once

The existing route's `expression_type` types both operands of a binary operation, and then types them again after the numeric context has been applied, and every ancestor does the same to its subtree. For a left-nested chain such as `a + b + c + ...` that is exponential in the number of terms, so the existing route does not finish a 40-term sum. AIR now remembers the inferred type on each binary node, which is safe because it depends only on the node and on the routine's fixed local and callee types, and it reuses an operand's type when the context left that operand unchanged. A retyped literal path is a new node with nothing remembered. Only binary nodes are remembered, and the stored type is boxed, because typing a leaf is cheaper than looking it up and an unused slot should stay small; remembering every node cost about 1.5% on the shallow expressions of the benchmark. A 300-term chain now compiles in about 0.1 s on the migrated route, and the parity tests compare the two routes at a depth the existing route survives.

### Slice 9 status: value-returning procedures, and a coverage report

A `proc` may return a scalar, and a call statement may discard the result of any routine that has one: the call is lowered with a destination that is never read, as the existing route does. The linker's contract check now requires a value call to name a callee that returns something, and lets a statement call name any plain callee.

`tests/pipeline_coverage.rs` reports how much real code the pipeline accepts and why it declines the rest (`COVERAGE_VERBOSE=1` lists each decline; the test asserts a floor, so a change that makes the pipeline decline more is noticed). On the 58 routines in the examples and std sources, 9 are accepted. The reasons for the rest, by count: reference parameters (12), generic routines (8, which are specialized to concrete copies before lowering and never reach this pipeline as indexed bodies), collection, option and result types (8), `gc` and `rc` types (8), record types (3), module variables and function values (4), and one each of string, array, closure and index expressions. Every one of these needs more than typing: a reference or an owned aggregate brings the ownership and loan checks (`ownership.rs` walks the owned AST body, which the pipeline no longer has), drop scopes, and the place and lookup operations. Extending the subset further is therefore a series of ownership-aware slices, each to be verified against the existing route by the differential generator, not more scalar forms.

### Slice 10 status: scalar references

A routine may take `&T` or `&mut T` parameters over `i64`, `bool`, `f64` and the sized numbers, and a call may borrow a local. This slice is verified by its own tests (`tests/scalar_references.rs`, with the Cyallo program `examples/references/scalar.yas`), which state what the language does and do not compare with the existing route. The rules are small and are checked in AIR:

- `p.*` reads and `p.* = v`, `p.* op= v` write through a reference parameter. Writing through `&T` is E107 ("cannot write through a read-only reference").
- `&x` and `&mut x` may appear only as direct call arguments, and `x` must be a local or a by-value parameter, not a reference. `&mut x` of a local declared without `mut` is E107 ("cannot modify a constant"). A reference to a reference is rejected.
- A reference parameter may be passed on as an argument; a `&mut` may be passed where `&` is expected, and not the reverse (E109).
- Within one call, two loans of the same place conflict unless both are shared (E118, "call arguments overlap and include a mutable reference"). Loans exist only for the arguments of a call, so no loan outlives it and nothing needs lifetimes.
- A reference is never a value: it cannot be stored in a local, returned, compared or used in arithmetic, and a result of reference type, a named lifetime, or a reference to anything but a scalar leaves the routine to the existing route.

The emitted IR has the shape the rest of the compiler already reads: a borrow is the local's address moved into a reference-typed register, a dereference loads or stores through that register, and storage placement then keeps a borrowed local in a cell. Purity needs no new code, because the effect analysis sees the store through the reference. `tests/pipeline_coverage.rs` now reports 16 of 64 routines accepted.

This slice also makes one earlier claim true: calls now read the callee's parameter list through the callee table instead of copying it. An earlier edit that was meant to do this had silently not applied, so the change recorded as clone removal was only the ownership move and the borrowing `contextualize`.

### Slice 11 status: strings

`string` parameters, results, locals and literals, concatenation (`+`, `+=`), comparison, and `&string` and `&mut string` references now lower through the pipeline. Verified by `tests/strings.rs` and `examples/strings/basic.yas`.

A `string` is an owned value that is not `Copy`, and that is what this slice adds. Handing a string on moves it out of its variable: the root of a `let` initializer, an assignment, a return value, or a by-value call argument. Reading it does not: the operands of `+` and of a comparison, and a shared reference's `p.*`. A moved variable may not be used until it is written again. AIR emits a moving read as `Address` and `Take`, which empties the local (the legacy interpreter really does), and everything else as `Load`. A forward dataflow over the typed blocks (`check_moves` in `air.rs`) tracks which locals may have been moved at the start of each block, merging across branches and back edges, and reports the first use of one (E118, "use of a moved value", with a related note at the move). Because it works on blocks it needs no special case for `if` or loops: a loop that moves an outer string reports the move at the next pass's use. Moving out of a reference (`return s.*` for `s: &string`) is E118 ("cannot move an owned value out of borrowed or shared storage; clone it explicitly").

Strings have no destructor, so there are no drop scopes yet; the move check is the whole of the ownership model for this slice. Records and anything with a destructor will need drops, which the same dataflow can drive. Printing and string methods are intrinsic calls and are the next big source of declined routines (`print` is how most procedures end). `tests/pipeline_coverage.rs` now reports 24 of 70 routines accepted.

### Slice 12 status: records

Records without type parameters, whose fields are scalars, strings or other such records, now lower through the pipeline: a literal (`Point { y: 2, x: 1 }`, fields in any order), `p.a.b` reads and writes on a local, a by-value parameter or a reference parameter, and records as parameters, results and call arguments. Verified by `tests/records.rs` and `examples/records/basic.yas`; each test also reads a per-thread counter (`bodies_emitted_on_this_thread`, with one compile job) to check that every routine eligible for the pipeline was lowered by it and did not quietly fall back at link time.

The new difficulty is that HIR works from names it cannot resolve. A parameter of type `Point` is only a spelling there. The module graph therefore decides eligibility at link time (`pipeline_signature` in `namespace.rs`): it resolves each parameter and result type in the routine's own module and accepts scalars, strings, records whose resolved fields are all acceptable, and references to those. The record table is built before any body is bound, so the order of modules does not matter. A record literal is a reference site of a second kind, bound to the record's linked name; calls and record literals share one overlay. At analysis time AIR looks a record's definition up in the type environment, and expands a type's name into its fields (`view`) wherever it needs them: parameter types, callee types, and field places. A local annotated with a record type is left to the existing route, because its annotation cannot be resolved in HIR.

Rules, all checked in AIR. A record is not `Copy`, so handing a whole record on moves it, and the move dataflow of the strings slice applies unchanged: a field read or write counts as a use of its root, so reading a field of a moved record is E118. Reading a field of a non-`Copy` type in a position that moves it (a `let`, a return, a by-value argument) is E118 ("cannot move a field out of a record; move the whole record or read the field in place"), which keeps partial moves out of the model; reading it in place (an operand of `+`, a comparison) is fine. Writing a field needs a root declared with `mut`, or a `&mut` reference (E107). A literal must name each field once (E101 for a repeat), no field may be unknown or missing (E116), and each value is checked against its field's type with the same typing rules as any other expression, with literals typed by their context. Literals evaluate their fields in the order written and gather them in declaration order, as the existing route does.

The emitted IR is `Aggregate` for a literal and, for a field place, the root's address (followed through a reference when the root is one) and one `Field` projection per step, then a `Load` or `Store`. `tests/pipeline_coverage.rs` now reports 32 of 78 routines accepted.

### Slice 13 status: enums and `match`

Enums without type parameters whose payloads are `Copy` scalars (or that have none) now lower through the pipeline: unit variants (`Empty`), payload constructors (`Circle(r)`), and `match` over a local or a call's result with variant arms (payload ignored with `_` or bound to a new local), wildcard arms, and arm bodies that are single expressions (which may themselves hold `&&`, `||` or another `match`). Verified by `tests/enums.rs` and `examples/enums/basic.yas`, which check the route with the same per-thread counter as records.

Construction is in place: the linker binds a variant constructor to a site of a third kind (`Site::Variant`, found from the generated `$variant..` function's signature and the variant's source name), and AIR emits `EnumConstruct` with the payload operand, so no call is made and no function id is demanded. A bare name that is not a local becomes a name-use site in HIR, which the module graph accepts only as a unit variant and otherwise sends the routine to the existing route.

A `match` ends blocks mid-expression like `&&`, so it may sit only where a short-circuit may (not under an operator or as a call argument). It becomes, for each arm, a test block ending in a branch (a wildcard arm's test block is empty) and a body block ending in a jump, followed by one block holding `Unreachable`; that is `2n + 1` extra blocks, counted syntactically (`extra_blocks`) so jump targets are known before any block is built. The value of the arm that runs is gathered in a hidden local, because values do not cross blocks. A local scrutinee is tested in place and stays usable; a call's result is first held in a hidden local. Tests and payload reads address that local (`EnumTest`, `EnumPayload` by value), as the existing route does for copied payloads. Bindings are typed from the scrutinee's enum in `infer_locals`, together with the match's hidden locals, so a `let` initialized by a `match` is inferred like any other.

Errors are E185 as before: a missing variant lists what is missing, an unknown or foreign variant, a payload pattern that does not match the declaration, and arms of different types. A moved scrutinee is E118. Nested or tuple patterns, `or` patterns inside a payload, and references stay with the existing route. Two stack-depth fixes came with it: `contextualize` and the new match walker now recurse under `with_stack`, since a 300-term chain overflowed a debug build's stack once frames grew. `tests/pipeline_coverage.rs` now reports 36 of 81 routines accepted.

#### Non-`Copy` payloads

Payloads that are strings, records or other enums are accepted too. A binding of a non-`Copy` payload owns it: AIR emits `EnumTake` (the payload's address, then `Take`, as the existing route does) and `Consume` on the matched local, which the move dataflow treats as a move on that path only. So a later `match` or read of the local is E118 if any arm that ran may have bound a payload, while an arm that ignores the payload with `_` leaves the value intact. Strings, records of scalars and strings, and enums of those have no destructor, so no drop is emitted; types that need one remain outside the pipeline. Checked with `--gc-stress`.

#### Guards

An arm may have `if condition`. The guard runs in its own block after the arm's test, with the payload binding in scope, and a failed guard (like a failed test) goes to the next arm, so a variant may appear in several arms. A guarded arm does not count towards exhaustiveness (`_ if true` is not a wildcard). A payload that is not `Copy` is read in place for the guard (an alias, without moving it) and only taken into the binding after the guard has chosen the arm; a guard that would move it is E185 ("a guard cannot move a matched value before its arm is selected"), and a guard that is not a boolean is E185 as well. A guarded arm costs one more block, plus whatever its guard needs, in the syntactic block count.

#### Block arms and statement matches

An arm may be a block, and a `match` may be a statement. Arms that are expressions only stay inside the expression (above). As soon as one arm is a block, or the match stands alone as a statement, the match is a statement in HIR (`StatementKind::Match`), because an arm's statements need real control flow. A value match is hoisted out of its `let`, assignment or `return`: the match statement assigns each arm's value, or its block's tail, to a hidden local, and the original statement reads that local. MIR then lowers the match to blocks like any other statement: a `MatchCover` statement carries the arms for one check of unknown variants, payload shapes and exhaustiveness; each arm gets an `Exit::MatchTest` block, an optional `Exit::Guard` block, `MatchBind` statements (staged `Direct`, or `Alias` before a guard and `Take` after it) and its body; a failed test or guard goes to the next arm, and falling off the end is `Exit::Unreachable`. Because every piece is an ordinary MIR block, AIR needs no block counting of its own for these, and `break`, `continue` and `return` in an arm are plain jumps. A statement match's expression arms must be calls (made for their effects); a block that yields no value in a value match must end in `return`, `break` or `continue`. Assigning a match in a compound assignment or using it under an operator still leaves the routine to the existing route.

#### Or-patterns

`A | B(n) | C(n) => ...` at the top of an arm. A match with an or-pattern is a statement match (see above), because each alternative needs a test and binding block of its own. MIR gives every alternative a `MatchTest` block and a bind block; a failed test goes to the next alternative, a matched one binds and jumps to a shared block, and the guard and body follow there. Alternatives must bind the same name, which is one local (HIR reuses the slot, so a second alternative's `n` is the first's); a payload type that differs between alternatives is E185 ("pattern alternatives must bind the same names with the same types"), found when AIR gives the shared local its type. Each alternative counts for exhaustiveness unless the arm is guarded. A guard on an or-pattern that binds a payload, and alternatives that bind different names, leave the routine to the existing route, because a guard cannot know which alternative's payload to hand on. `tests/enums.rs` and `examples/enums/or_patterns.yas` cover it.

### Slice 14 status: `print`

`print` as a statement lowers through the pipeline. The module graph resolves the path and, when the symbol is the `print` intrinsic used as a statement, records the marker target `$print` in the overlay instead of a callee; `bind_callees` turns it into `Site::Print`, which has no function id, so nothing is demanded or called. AIR evaluates every argument in order (reads, not moves, so printing a string leaves it usable), then requires each type to be a number, string, char or boolean, and otherwise reports the same E112 as the existing route (`` `print` does not accept (A, B) ``). It emits `Output` with the argument values. Purity is unchanged: a `fn` that prints is E120 as before, so printing is exercised in procedures. `tests/print.rs` and `examples/print/basic.yas` cover it, each checking the route with the lowering counter. Other intrinsics are not in the pipeline: their lowering depends on resolved generic types (`resolve_declared`), which a later slice has to bring over, and they still send the routine to the existing route at link time. `tests/pipeline_coverage.rs` now reports 39 of 84 routines accepted.

### Slice 15 status: utility intrinsics

The string and numeric helpers (`std::string::len`, `trim`, `upper`, `lower`, `contains`, `starts_with`, `ends_with`, `replace`, `from`; `std::num::abs`, `min`, `max`, `clamp`, `round`, `floor`, `ceil`, `sqrt`, `pow`; `std::char::code` and `from_code`) lower through the pipeline. They are the intrinsics the existing route resolves from operand types alone (`builtins::result_type`) and emits as `Builtin`, so no resolved generic is needed. `builtins::pipeline_utility` lists them; the module graph records a `$builtin:<id>` marker in the overlay and `bind_callees` makes a `Site::Builtin`. AIR reads each argument (strings are not moved), contextualizes numeric literals to the type the other arguments share (the rule `intrinsic_arguments` uses: first non-literal argument, else a real literal, else the first), then asks `result_type`; a type it rejects is E112 `` `name` does not accept (A, B) ``, as before. A utility is an ordinary expression, and as a statement its result is dropped. `tests/builtins.rs` and `examples/builtins/basic.yas` cover it. Intrinsics whose lowering needs resolved generics or a native contract (collections, `clone`, `rc`, `gc`, channels, services, `input`, `Option`/`Result` constructors) still send the routine to the existing route, and the verified backend does not lower every utility (`clamp` and `sqrt` report R900 on both routes), which is a backend gap, not a pipeline one. `tests/pipeline_coverage.rs` now reports 44 of 89 routines accepted.

### Slice 16 status: generic enum instances

A routine that is not itself generic may now take, return, match and construct instances of a generic enum (`Maybe<i64>`, `Either<i64, string>`, nested instances). HIR no longer rejects an applied type in a signature (it never used the type); the module graph decides, from the resolved signature, with a table of generic enums built beside the one for plain enums. An `Applied(enum, arguments)` type is accepted when each argument is a pipeline type and each payload, with the enum's parameters replaced by the arguments, is too. The instance in AIR is the ordinary `Type::Enum` the signature resolves to, so matching needs nothing new: the payload types are already substituted.

Constructing is the hard part, because `Some(n)` does not say which instance it builds. The existing route fixes it from context (`constructor_context`: an annotation or a returned type); the pipeline does the same, from the expected type. HIR marks a site `contextual` when it is a returned value or a call argument, the places whose type is known to AIR, and the module graph accepts a generic variant constructor only at such a site. AIR binds it as `Site::GenericVariant` (the enum's declaration id and the variant name, read from the constructor symbol) and `expected_as` builds it from the expected type: the instance is the wanted type, the variant and its payload type come from that type's contract, and a wanted type of another enum is E109. With no expected type (`infer`) the variant has no type, which cannot happen for an accepted site; an unannotated `let m = Some(3)` is not contextual, so its routine stays on the existing route. The standard `Option` and `Result` work as before (their constructors are `Storage` types, not accepted here yet). `tests/generics.rs` covers it.

#### Generic routines

A generic routine is checked once, as a template, with its type parameters opaque, and not once per instance. This is the rule of Rust and not of C++: `T` tells the body nothing, so `fn first<T>(val: T) { match val { Foo::Bar(a) => a } }` is an error in the template, and stays one for `first<Foo>(foo)`. HIR accepts a routine with type parameters (it only spells them in the signature; bounds and packs leave the routine to the existing route). At link time the module graph checks the signature with each parameter stood in for by a scalar, and a bound template keeps its indexed body beside an owned body, which the specializer clones for instances the pipeline does not take. The linker also records the template's linked parameters and result, with the parameter names left as `Type::Named`, in the body (`GenericHeader`). Before any instance, module analysis runs AIR once over each template with those opaque types (`check_generic`, shared through the body's `OnceLock`, in span order, whether or not the template is used): an opaque value is not `Copy`, so reading it twice is E118, and it is not a number or an enum, so arithmetic is E105 and matching it is E185 ("variant pattern requires an enum, found `T`"). A callee that takes `T` is a generic callee, which the module graph does not bind, so such a template stays on the existing route.

Instances: the specializer, which still instantiates templates by substituting type arguments into a cloned tree, registers an instance in the indexed bodies under its own name when every type argument is a scalar or a string, and clears its tree. The instance is then emitted from the template's body and the instance's own, substituted, parameter types. That second typing cannot reject what the first accepted, because the opaque check permits only operations valid for every type. An instance with any other type argument keeps the cloned tree and the existing route, which still checks the instance's body with concrete types; the template check above has already run, so such an instance is no longer more permissive than the template. `tests/generics_routines.rs` covers it, including the counter that shows the instances were lowered by the pipeline. 

#### Trait bounds

Until now a bound on a source routine (`fn f<T: Numeric>`) was rejected as unimplemented (E173); the pipeline gives a set of them meaning. A bound is what a parameter's type tells the body, and nothing more. The single traits are the operators and a few facts: `Add`, `Sub`, `Mul`, `Div`, `Rem` and `Neg` (`std::ops`, each `Self op Self -> Self`), `Eq` and `Ord` (`std::cmp`, giving `==`/`!=` and `<`/`<=`/`>`/`>=`), `Copy` (`std::mem`, values are duplicated by reading them, not moved), and `Format` and `Debug` (`std::fmt`, the plain text form that `print` writes and the form meant for a programmer; `Format` replaces the old `Printable`). `Numeric`, `Integer` and `Floating` (`std::num`) are bundles: every operator but `Neg`, comparison and `Copy`, over any number, the integers, or the floats. Bounds combine with `+`. A type argument must satisfy every bound (scalars and strings only for now, and `Neg` is not satisfied by an unsigned integer), and a bound gives only its own capability: `a - b` under `T: Add` is E105, a comparison under `T: Eq` and not `Ord` is E105, a bare `T` is moved where a `T: Copy` is copied, and printing needs `Format` (`Debug` has no consumer yet, so printing under it is E112). Any other trait, a bound that does not resolve to one of these, or a routine the pipeline does not take, stays E173.

The module graph resolves each bound to its linked trait (a table built from the standard modules) and keeps the result in the template's header. The template check (above) runs with those bounds; each instance is checked against them when it is made (E151, "type argument `string` does not satisfy the bound `Neg`"), by the specializer, on whichever route lowers it. Every type that satisfies one of these bounds is a scalar or a string, so every valid instance is lowered by the pipeline from the template's body.`tests/pipeline_coverage.rs` now reports 55 of 97 routines accepted.

### Names are interned ids, not copied text

HIR, MIR and AIR no longer copy names. The indexed syntax interns every identifier and string literal once per parse unit as a `NameId`, and the retained body already keeps its unit alive, so the pipeline carries the id: a reference site's path, a record literal's field names, the names in a field place such as `p.a.b`, a local's name, and the text of a string literal (`ExpressionKind::Text`). A local the lowering makes up (a loop's hidden counter) is `LocalName::Hidden`. The text is read only where it is needed: the module graph binding a site path, AIR comparing a field name with a record definition or writing a diagnostic, and emission, which turns a local's name into the owned text that `ir::Local` keeps. A string literal is the one place no copy happens at all: its interned `Arc<str>` goes straight into `LiteralString`. Names from different units have different ids, so a field is compared with a record declared in another file by its text, as before. A span would not do for string literals, because a literal's value differs from its source text when it contains escapes; the interned text is already unescaped. The effect on first output is small (about 1 to 1.5% at 1,024 to 16,384 routines, paired), since HIR construction was never the largest cost.

### Hashing: BLAKE3

Every digest in the compiler (the module-cache key, body digests and SDK interface digests) is BLAKE3; `sha2` is no longer a dependency of `cyallo-ir`, and a `Digest` now displays as `blake3:...`. Paired against the build before the change, first output for unused routines improved by about 3.5% (68.4 ms to 66.0 ms at 4,096, 263 ms to 254 ms at 16,384). The `xtask` provenance check for the vendored Winch sources still uses SHA-256, because it verifies the checksum of an upstream tarball published as SHA-256 and the manifest would have to be re-pinned from that tarball.

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
