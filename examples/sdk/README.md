# SDKs and worlds

Run these from the repository root:

```sh
cargo run -- run examples/sdk/worlds.yas --engine interpreter
cargo run -- run examples/sdk/unbound.ya --engine interpreter
cargo run -- run examples/sdk/prelude.yas --engine interpreter
```

| Example | Demonstrates | Expected output |
| --- | --- | --- |
| `worlds.ya` | `interface mod` with opaque types, `impl interface` bindings, worlds with `include`, member-listed `import` and `export` | `data` |
| `prelude.ya` | The runtime's `prelude` SDK: `use prelude::core::*` brings in `Option`, `Some`, `None`, `Result`, `Ok`, `Err` | `4`, `none` |
| `unbound.ya` | An opaque type left unbound is rejected | a diagnostic that `Logger` is left unbound |

`prelude::core` lives in `std/prelude.ya` as plain reexports (no compiler status). Only the
`core` group exists so far; `prelude::collections` and `prelude::tasks` arrive when those
SDKs are split out of `std`.

What runs today is parsing and resolution. A binding makes the opaque type name the
implementing type inside the program, and worlds are checked: includes are expanded,
import members are matched against the real signatures, and exports are resolved.
Generating `W::Env`/`W::load`, instantiating a component against a world, and the
runtime-provided SDKs (`core`, `collections`, `tasks`) are later phases; the output
above comes from ordinary code after the checks pass. Interface digests and `#[since]`
rules are available to tools through `ResolvedProgram.interface`.
