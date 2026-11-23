# Data enums

Run these from the repository root:

```sh
cargo run -- run examples/enums/result.yas --engine interpreter
cargo run -- run examples/enums/patterns.yas --gc-stress
cargo run -- run examples/enums/ownership.yas --engine cranelift_speed --gc-stress
cargo run -- run examples/enums/recursive.yas --gc-stress
```

| Example | Demonstrates | Expected output |
| --- | --- | --- |
| `result.ya` | Standard Result variants and structural error payloads | `4`, `cannot divide by 0` |
| `patterns.ya` | Generic variants, guards, alternatives, borrowed patterns and mutation | `positive: 7`, `changed`, `2` |
| `ownership.ya` | Failed guards preserve ownership; selected arms move one part and release the remainder | `first:1:1:true` |
| `recursive.ya` | Generic recursive enums with managed indirection | `5` |

Comma-separated expected outputs above are separate lines. Variant constructors
are callable values; unit variants are values. For a record payload,
`Variant { fields }` supplies that record to the constructor. Ordinary anonymous
records remain structural and do not coerce to a declared record.

Matching a value moves its non-copy bindings. Matching `&value` or `&mut value`
binds references to its actual payload storage. A guard can read those bindings;
moves and writes to the matched storage wait until an arm is selected. Unguarded
patterns must cover every possibility; outside its declaring package, an open
enum additionally requires a wildcard arm.
