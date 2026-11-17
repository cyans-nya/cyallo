# Module compilation

Modern compilation retains a resolved module graph through semantic checking and
IR generation. A module owns its source declarations, checked routines, global
storage segment and optional initializer. Imports expose declarations with their
original `DeclarationId` and defining `ModuleId`; aliases and reexports retain
those identities. Visibility and extension imports are checked by the resolver.
See [module syntax and services](services.md) for the language interface.

The [executable example](../examples/modules/static/main.ya) combines imported
records and traits, a private helper, dependent constants, a generic function and
a returned capturing closure:

```sh
cargo run -- check examples/modules/static/main.ya --jobs 4
cargo run -- run examples/modules/static/main.ya --gc-stress
cargo run -- run examples/modules/static/main.ya --engine cranelift_speed --gc-stress
```

It prints `42:retained`.

## Checking and linking

1. Files are tokenized independently. Declaration/body parse jobs share the worker
   budget across files. The combined source archive supplies diagnostic offsets;
   it is not parsed as a combined program.
2. The resolver builds module interfaces and resolves imports, aliases, privacy
   and native declaration permissions. Each resolved module retains its own AST.
3. Generic instances stay with their template's defining module. Type, trait,
   signature and global binding catalogues share contracts across modules.
   Compile-time constants follow their declaration dependency graph; cycles fail.
4. Once contracts and executable slots are frozen, independent initializer and
   function jobs perform ownership/effect checking and typed IR lowering. A single
   large module can use multiple workers. Results and diagnostics retain module
   and declaration order. Deferred routine checks use the same contracts.
5. Linking assembles the executable routine and global tables from checked module
   products. The entry routine receives IR calls to imported initializers; source
   statements are never copied into the entry AST. Whole-program effect propagation
   runs over the linked call graph during eager checking.

Imported module initialization currently permits compile-time constants. Runtime
initializers materialize those checked values in module-owned slots before entry;
side effects belong in explicit procedures. Session storage and initialization
state are independent even when Sessions share checked code.

`Program::modules()` exposes the retained `CheckedModule` products: identity,
resolved bindings, global slots, initializer and owned routines. Executable routine
and storage indices are link-layout coordinates. They are not source declaration
identities, and may move between compilations.

## Reusing checked products

An embedder can retain `modules::Compiler::default()` and call its `compile` or
`compile_with_policy` methods repeatedly. The compiler owns its cache; Programs
retain that cache for runtime loading. Standalone `compile_modules` calls create
independent compiler instances. No process-wide cache or runtime registry exists.

A cache key includes the module's normalized source, referenced declaration/type
contracts, native declarations and scoped permissions. Changes to a callee body
with an unchanged interface can reuse its callers' checked routines. Changes to
imported signatures or layouts invalidate dependent products. Constants and
instantiated generic bodies participate through their transformed owner modules.
`CheckedModule::build` reports `Compiled` or `Reused` for each invocation.

Reused routines relocate calls, function values, globals, trait implementations,
service keys and source spans into the new executable layout. Source coordinates
use file identity and relative offsets. Relocation is lazy for deferred bodies;
previously checked bodies and cached diagnostics remain shareable. Generated
method/closure names do not depend on source archive offsets.

The current cache is in memory and retains compilation history for the lifetime
of its compiler. It has no serialized artifact format, eviction policy or stable
external binary ABI. Parsing, namespace resolution, generic/type preparation and
constant evaluation still run before cache selection. Changes to the shared
native catalogue can conservatively invalidate additional modules. This is
reuse of checked module IR, not a claim of a fully incremental front end or of
independently distributed compilation.

## Runtime publication

[Runtime loads](dynamic-import.md) use the same module checking and cache. A
publication links new routines and global segments into an append-only snapshot,
then invokes only newly introduced module initializers. It preserves existing
routine IDs, suspended frames and Session state. It does not slice statements
from a flattened entry body or replay the caller's top-level code. Resolution and
contract preparation still rebuild the extended package context.

Verification lives in [module product tests](../tests/module_units.rs), alongside
[namespace tests](../tests/namespace.rs), [module tests](../tests/modules.rs),
[parallel compilation tests](../tests/parallel_compilation.rs) and
[runtime loading tests](../tests/dynamic_import.rs). The example is exercised with
eager/deferred checks, interpreter/Winch/Cranelift, moving GC stress and
single-instruction handoffs. Cache tests verify changed interfaces, moved routine
slots, source relocation, private template helpers and independent Session state.
