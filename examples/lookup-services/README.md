# A service registry in user code

```sh
cargo run -- run examples/lookup-services/main.ya --gc-stress --jobs 4
```

```text
first: ready
second: replacement
second: cached lookup
FALSE
```

Start with [main.ya](main.ya). Importing `logger` enables `ctx.logger` in
that module. Calling `install` activates the console provider. The retained
`live` handle sees its replacement, and disposing the first lease leaves the
replacement available. Disposing the second lease makes `available` return false.

[logging.ya](logging.ya) defines the trait and typed key, then an ordinary
extension getter that calls `service`. Its `ServiceHandle<dyn Logger>` trait
implementation calls `acquire` for each message. [console.ya](console.ya)
implements the concrete provider and exposes an explicit installation procedure.

[registry.ya](registry.ya) separates the directory from binding contents:

- `layout` holds a Copy handle to an immutable GC snapshot inside an owned `Cell`. The
  readonly mapper scans it on a cache miss and returns a relative slot recipe.
- `entries` holds an immutable `@gc Vec<@gc Cell<@gc Binding>>` snapshot.
  A handle retains a managed cell owner. Each acquisition copies the current
  binding snapshot's GC handle and checks the provider's erased type.
- A lease records the binding generation it installed. Disposal only clears a
  matching generation, preventing an old lease from removing a replacement.

A new key changes the directory version. Replacing or removing a provider
changes its binding contents and preserves the directory, cell, and storage path.
The example uses no builtin service operations; these policies are implemented
by its records and routines using the general lookup/storage primitives.

To explore the behavior, change the message prefixes or add a third `install`
after disposing the second lease. Calling `live.info` then uses that newly
installed provider through the original handle. Calling it while unavailable
stops with `service unavailable`.

For the smaller lookup mechanism alone, read [lookups.ya](../lookups.ya).
For injection and dependent cleanup hooks, run the [Context lifecycle example](../services/main.ya).
The complete primitive contracts are in [the lookup reference](../../docs/lookups.md).
