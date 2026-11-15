# Open objects

Run each `.ya` file with `cargo run -- run <path> --gc-stress`. Each also works
with `--engine jit` (the tiered Winch path), `--engine cranelift`, and `--engine cranelift_speed`.

| Example | Behavior | Expected output (lines) |
| --- | --- | --- |
| [open.ya](open.ya) | Required typed fields, aliases, configurable fields, deletion and compound writes | `Grace:true`, `ten:true`, `Grace:11` |
| [views.ya](views.ya) | Structural checks retain identity/extra fields; dynamic native calls | `true`, `Grace:7`, `42` |
| [native-ic.ya](native-ic.ya) | Repeated shape-guarded native scalar reads/writes | `19999900000` |

The [loading example](../modules/dynamic/main.ya) uses the same property/call
operations for a readonly module namespace. See [objects](../../docs/objects.md)
for ownership rules and [runtime loading](../../docs/dynamic-import.md) for grants.
