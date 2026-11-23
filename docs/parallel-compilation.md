# Parallel compiler units

The front end now has independent parse and routine compilation units. The
compiler uses available CPU cores by default. `CompileOptions.jobs` and the CLI
`--jobs` select the worker budget: `0` is automatic (at most 32), `1` is serial,
and explicit budgets are capped at 128. Tiny stages execute serially to avoid
thread startup overhead.

```sh
cargo run -- check examples/lookup-services/main.yas --jobs 4
cargo run -- bytecode examples/lookup-services/main.yas --jobs 1
```

| Stage | Current scheduling |
| --- | --- |
| Source archive and import discovery | Ordered file discovery/archive offsets |
| Lexing | Independent across files; sequential within one file |
| Modern token indexing | One linear delimiter scan per file |
| Modern AST parsing | Independent declaration/statement units within a file; independent files |
| Import/export linking and constant order | Ordered dependency barrier |
| Generic specialization, composite resolution, closure conversion | Ordered declaration/type preparation |
| Fixed-type inference, semantic checks, typed body IR | Independent module initializer and function jobs after contracts are frozen |
| Whole-program effect propagation | Coordinated call-graph fixed point |
| Local storage placement, GC metadata, liveness/resume metadata | Independent function jobs |
| Register bytecode translation | Independent checked function jobs |
| Deferred analysis and translation | Per-function shared `OnceLock`, safe to request concurrently |
| Winch / Cranelift compilation | Existing separate bounded background worker service |

Not every stage can run independently: a body needs a stable signature table,
constant initialization has dependencies, and recursive call effects need a
fixed point. These barriers are explicit. No worker assigns global routine IDs
or publishes declarations in completion order. Results and the first reported
error are selected in source/unit order, regardless of scheduling.

The reusable `cyallo-compile-jobs` crate provides scoped, bounded jobs, dynamic
work distribution for heterogeneous units, and deterministic ordered results.
Nested stages reuse their worker rather than spawning nested pools. Module preparation lexes/indexes files independently, then schedules declaration
parse descriptors from every file in the same job wave. ASTs remain module-owned. A large module can therefore use
all workers even when its imports are small. Mutable per-function metadata jobs receive disjoint
slices. Inputs and shared declaration tables are immutable; there is no global
semantic lock.

Modern parser units share a single tokenized source and borrow the original
UTF-8 text. Each task owns a token fragment and AST while retaining original
file spans. A routine body, including its nested closures, is one parse unit;
traits and implementations are currently grouped with their member methods.
Incomplete/unbalanced delimiters use the single-unit parser to retain diagnostics.
Modern units reject CAIE syntax; compatibility source uses its own parser.
Lexing a single enormous function, or parsing that function's interior, remains
serial. These are limits of the present implementation, not claims of automatic
parallelism at every AST node.

Module initializers, the entry body and routine bodies share a checking job wave.
Imports, generic instances, closure identities, and signatures are finalized
before those workers start. Checked module products retain their own routines
and are linked afterward; see [module compilation and reuse](modules.md).
Generic specialization may perform provisional inference to discover argument
types; generic programs can therefore do more eager front-end work than programs
without templates. Detailed checked IR and bytecode of unused routine units still
remain deferred in execution mode.

## Preparing independent units

```rust
use cyallo::{Jobs, compile_for_execution};

let program = compile_for_execution(
    "fn first() -> i64 { return 1 }\nfn second() -> i64 { return 2 }\n",
    Default::default(),
)?;
// Unit zero is the entry body. Unselected bodies remain cold.
program.prepare_units(&[1, 2], Jobs(4))?;
for unit in program.compilation_units() {
    let name = &unit.routine.name; // cheap declaration access
    if unit.routine.is_analyzed() {
        let code = unit.bytecode()?;
        println!("{name}: {} words", code.instructions.len());
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`Program::validate_all_with_jobs` checks all bodies with a chosen budget.
`CompilationUnit::checked_ir()` and `bytecode()` prepare just that unit.
Concurrent callers share the same result and diagnostic, including recursive
routine groups: checking a body consults frozen callee declarations instead of
recursively acquiring other body locks. This avoids initialization lock cycles.

Tests compare serial/parallel bytecode, function IDs, execution, source positions,
and error selection; exercise concurrent preparation of the same units; compare
indexed parsing to the original parser; and verify that independent jobs actually
overlap. No wall-clock speedup is claimed without workload-specific measurement.
