# Examples

Run commands from the repository root. Modern examples use `.ya`; CAIE compatibility examples use `.psc`. The extension selects the dialect:

```sh
cargo run -- run examples/tour.ya
cargo run -- check examples/generics.ya --jobs 4
cargo run -- run examples/lookups.ya --engine cranelift_speed --gc-stress
```

`--engine interpreter` runs without native compilation. `--engine jit` is the
default; `cranelift` and `cranelift_speed` compile eagerly. `--jobs` controls
front-end compilation workers. `--gc-stress` is useful on the short examples;
`--gc` runs the longer allocation workload with normal collection intervals.

## Concurrency runtime sets

The [dedicated concurrency examples](concurrency/README.md) are Cyallo `.ya`
programs. The generator set covers factories, captures, resume input, completion,
and delegation, with run commands and expected output:

```sh
cargo run -- run examples/concurrency/generators/sequence.ya
cargo test --test generators
```

Task, channel, and managed transport integration is still in progress.

## Start here

Read these in order, or choose the feature you want to try. Each standalone file includes
its run command and comments explaining the behavior.

| Example | What it demonstrates |
| --- | --- |
| [tour.ya](tour.ya) | Small functions, procedures, arrays, range loops, break/continue, and string utilities |
| [composites.ya](composites.ya) | Records, tuples, callbacks, returning owned closures, and explicit `&mut` captures |
| [generics.ya](generics.ya) | Generic records, inferred/explicit type arguments, an array transform, and specialized closures |
| [traits.ya](traits.ya) | Concrete method calls, `dyn` dispatch, owned conversion, and aliases to an erased object |
| [gc-sharing.ya](gc-sharing.ya) | `@gc` aliases, owned `Cell` fields, snapshots, and references retained across vector growth |
| [gc-cycles.ya](gc-cycles.ya) | Recursive records, strong circular GC references, and reclamation of unreachable cycles |
| [rc-ownership.ya](rc-ownership.ya) | `@rc` moves, explicit cloning, pure borrowed reads, weak upgrades, and last-owner destruction |
| [gc-rc.ya](gc-rc.ya) | Independent RC owners sharing one traced GC cell |
| [managed.ya](managed.ya) | Explicit RC ownership, weak references, and a custom allocator |
| [locations.ya](locations.ya) | Value snapshots, explicit GC sharing, and stable typed locations |
| [lookups.ya](lookups.ya) | A user mapper, cached misses, current-value loads, and directory invalidation |
| [lookup-services/main.ya](lookup-services/main.ya) | A service registry implemented in the language, imported extensions, replacement, and disposal |

Expected output of the tour:

```text
ADA
squares: 26
range sum: 10
names: 3
```

The composite example prints `1:7`, then `2`: the returned offset closure owns
its capture, while the increment callback borrows a counter for the duration of
`count_twice`. A borrowed closure cannot escape that counter's lifetime.

The generic example prints:

```text
items:3
item-1,item-3
42:hello
```

The trait example prints `direct: 5`, `shared: 9`, then `original: 5`. The example explicitly clones the concrete record before converting it to
`dyn Counter`, and explicitly clones the erased handle to share that instance.

The location example prints:

```text
snapshot: 2
field: 5
first: updated
other: second
```

The lookup example prints:

```text
red: 10
red: 10
red: 20
blue: missing
blue: missing
blue: 30
```

Repeated calls share one lookup site in `find`. Changing red's cell is visible
through the cached plan. Adding blue changes the directory version, allowing a
previously cached miss to resolve. See [the lookup reference](../docs/lookups.md)
for the primitives and cache contracts.

## GC and RC ownership

Start with the three short ownership examples:

```sh
cargo run -- run examples/gc-sharing.ya --gc-stress
cargo run -- run examples/rc-ownership.ya
cargo run -- run examples/gc-rc.ya --gc-stress
```

`@gc` and `@rc` enable their required tracing automatically, including without
`--gc`. Add `--gc-stress` to exercise collection during allocations. The older
[gc-cells.ya](gc-cells.ya) is an allocation workload; these examples demonstrate
the explicit ownership syntax.

[gc-sharing.ya](gc-sharing.ya) copies a GC reference and mutates its owned `Cell`
field. An integer snapshot keeps its old value. A retained managed reference
continues to share the same counter after vector growth and allocation pressure:

```text
snapshot: 10
shared: 11:11
retained: 21
```

Cell reads require Copy contents. `*counter.value = *counter.value + 1` replaces
the integer without exposing a mutable reference. For owned, non-Copy contents,
use `.replace(value)` to transfer the previous value out. A managed Cell adds a
dereference layer: `**state` accesses an `@gc Cell<i64>`.

[gc-cycles.ya](gc-cycles.ya) links two nodes with strong references in both
directions. Its cells hold Copy handles to GC-owned edge vectors. Once an
invocation returns, its unreachable cycle can be collected:

```sh
cargo run -- run examples/gc-cycles.ya --gc-stress --jit-report
```

```text
cycle: 10 -> 20 -> 10
temporary cycles: 128
```

The collector traces reachability, so circular references do not keep these nodes
alive. RC strong cycles require weak backedges instead.

[rc-ownership.ya](rc-ownership.ya) contrasts an explicit counted clone with a move.
Its pure reader borrows the owner. A weak upgrade creates another strong owner;
after all strong owners are dropped, upgrading fails:

```text
strong: 2
survivor: 42
upgraded: 42:2
expired: true
```

[gc-rc.ya](gc-rc.ya) gives two independent RC allocations references to the same
GC cell. Expiring one RC allocation leaves the other usable. The original GC
reference retains the cell after both RC owners expire:

```text
first: 7
second: 7
first expired: true
second survives: 9
gc survives: 11
```

Continue with [managed.ya](managed.ya) for a custom allocator whose deallocation
method records payload destruction. See [managed ownership and allocators](../docs/managed-memory.md)
for the full contracts.

## Services and modules

Run an entry file; the other `.ya` files in its directory are library modules:

```sh
cargo run -- run examples/lookup-services/main.ya --gc-stress --jobs 4
cargo run -- run examples/services/main.ya --gc-stress
```

The [user-defined registry walkthrough](lookup-services/README.md) follows these files:

| File | Responsibility |
| --- | --- |
| [registry.ya](lookup-services/registry.ya) | Typed keys/handles, a guarded mapper, stable binding cells, and generation-checked leases |
| [logging.ya](lookup-services/logging.ya) | The `Logger` trait, its key, the imported `ctx.logger` property, and forwarding |
| [console.ya](lookup-services/console.ya) | A concrete provider and explicit activation through `install` |
| [main.ya](lookup-services/main.ya) | Install, retain a handle, replace, dispose a stale lease, and remove the current provider |

Its output is:

```text
first: ready
second: replacement
second: cached lookup
false
```

The [Context lifecycle example](services/main.ya) uses the existing runtime
service API. It demonstrates injection and cleanup hooks, printing:

```text
first: ready
first: cleanup
second: ready
false
```

Imports expose declarations. The entry calls `install` to activate a provider.
Extension properties are selected by explicit imports. The registry example
implements replacement and disposal; the Context example also supplies admission
and dependent cleanup policies. See [the module/service reference](../docs/services.md).

## Algorithms and longer workloads

These use modern syntax. Run them with `cargo run -- run examples/<name>.ya`.
Size bindings remain easy to adjust; the engine benchmark uses the same files.

| Example | Topic | Default output |
| --- | --- | --- |
| [fib.ya](fib.ya) | Recursive Fibonacci | `17711` |
| [quicksort.ya](quicksort.ya) | Partitioning and recursive `ref` parameters | `513509887:1072:998045` |
| [mergesort.ya](mergesort.ya) | Stable merging with a scratch array | `398403790:1921:999272` |
| [dfs.ya](dfs.ya) | Iterative graph traversal, marking vertices when pushed | `300:0:300` |
| [knapsack.ya](knapsack.ya) | 0/1 and unbounded dynamic programming | `1054`, then `3400` |
| [parser.ya](parser.ya) | Repeated scanning of decimal tokens | `41600:12:330464` |
| [modern.ya](modern.ya) | Larger arithmetic/call workload | `ADA`, then `288212784234102776:10:3` |
| [hot-loop.ya](hot-loop.ya) | Scalar loop and invocation count | `5000050000:1` |
| [osr-loop.ya](osr-loop.ya) | Promotion within one long invocation | `500000500000:1` |
| [gc-cells.ya](gc-cells.ya) | Repeated array allocation/copying; run with `--gc` | `40000400001:200000` |

`std::collections::Vec::from(0..n)` creates a growable vector from a runtime
range. `std::array::from(0..4)` creates a fixed array with a statically known
length, and `std::array::empty<T>()` has type `[0]T`. DFS and knapsack initialize
all entries explicitly; growing a vector uses `push`.

## Interactive input

[secret-number.ya](secret-number.ya) is a modern guessing game with a
fixed secret of 42. It handles invalid text, fractional guesses, and out-of-range
values. Try it interactively or supply a reproducible session:

```sh
printf '%s\n' hello 2.5 20 70 42 | cargo run -- run examples/secret-number.ya
```

```text
Guess a whole number from 1 to 100.
Please enter a number.
Enter a whole number from 1 to 100.
Try higher.
Try lower.
Correct!
```

## CAIE syntax and migration

These `.psc` files use CAIE syntax, which modern mode rejects. Compatibility
mode is selected automatically. The declaration-free variant still requires
`--dialect simplified`.

| Example | Dialect | Input / output |
| --- | --- | --- |
| [loops.psc](loops.psc) | Compatibility (automatic) | Prints `5050` |
| [functions.psc](functions.psc) | Compatibility (automatic) | Prints `3628800` |
| [arrays.psc](arrays.psc) | Compatibility (automatic) | Prints `4,4` |
| [strings.psc](strings.psc) | Compatibility (automatic) | Prints `Cambridge9`; positions are one-based |
| [input.psc](input.psc) | Compatibility (automatic) | Prompts `Number?`; entering `42` prints `42` |
| [simplified.psc](simplified.psc) | `--dialect simplified` | Prints `3:11:3:123` |

```sh
cargo run -- run examples/arrays.psc
printf '42\n' | cargo run -- run examples/input.psc
cargo run -- run examples/simplified.psc --dialect simplified
```
