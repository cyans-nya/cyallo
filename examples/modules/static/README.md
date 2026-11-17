# Checked modules

Run from the repository root:

```sh
cargo run -- check examples/modules/static/main.ya --jobs 4
cargo run -- run examples/modules/static/main.ya --gc-stress
cargo run -- run examples/modules/static/main.ya --engine cranelift_speed --gc-stress
```

Output: `42:retained`.

`model` owns the record, trait and base constant. `math` imports those declarations,
keeps its helper private, evaluates a dependent constant, implements the imported
trait and owns the generic function and its capturing closure. `main` imports a
function under an alias and calls through a trait object and returned closure.
Each module keeps its own checked routines and initializer through linking.
