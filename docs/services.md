# Modules, typed services, and native call caches

The language supports Rust-style module paths and imports, extension properties,
traits with erased dispatch, and typed service contexts. Run the complete example:

```sh
cargo run -- run examples/services/main.ya --gc-stress --jit-trace=summary
cargo run -- check examples/services/main.ya
```

It prints `first: ready`, `first: cleanup`, `second: ready`, then `false`.

## Declarations and imports

`mod logging;` loads `logging.ya` or `logging/mod.ya`. Inline modules use
`mod logging { ... }`. `use crate::logging::{Logger, LOGGER, logger}` imports
declarations; `as` supplies an alias, `*` imports a module's public declarations,
and `pub use` reexports them. `crate`, `self`, and `super` anchor paths. Names are
case sensitive. Types, values, and extension properties retain distinct roles.
`pub` exports a declaration; unmarked declarations are private to their module
and its descendants. `pub(crate)` exposes a declaration within the compilation.

Only declared modules contribute code and constant initialization. Imports expose
declarations and do not activate providers. Dependency constants initialize in
order; cycles are errors. Other initialization belongs in an explicitly called
routine. `std::prelude` supplies ordinary module/type reexports.

Embedders can use `modules::compile_modules` with an explicit set of sources.
The CLI follows module declarations using `modules::load_path`. Source ranges
remain available in `Program::source_modules` for diagnostics and the debugger.

The [module compilation contract](modules.md) describes retained module IR,
initializers, executable linking and the instance-owned checked-module cache.

## Traits and extension properties

```text
use std::service::{Context, ServiceKey, ServiceRef}
pub trait Logger { proc info(self: &Self, message: string) }
pub const LOGGER = ServiceKey::new<dyn Logger>("app.logging.logger")
pub property Context.logger: ServiceRef<dyn Logger> {
    proc get() { return self.service(LOGGER) }
}
```

Traits declare checked method signatures. Implementations provide those methods
with matching value, shared-reference, or mutable-reference receiver contracts.
`value as dyn Logger` moves the value into an erased object with a dispatch-table
identity. Explicit `std::mem::clone(&value)` can retain a separate owned value.
Pure methods keep their effect contract through erased calls.

An imported extension property is statically resolved against its receiver type.
Actual record fields take precedence. Getter bodies preserve their ordinary
observable work. Extension properties add no fields. Generic traits, associated
types, default methods and extension setters remain future work.

## Context and lifecycle operations

| Expression | Result or behavior |
| --- | --- |
| `std::service::Context::new()` | Root context |
| `ctx.extend()` | Derived context inheriting admission |
| `ctx.isolate(KEY)` | Derived context with a fresh isolation for this key |
| `ctx.provide(KEY, value)` | Registration controlling a provider |
| `ctx.service(KEY)` | Typed live service reference |
| `ctx.available(KEY)` | Current availability under this context's admission |
| `ctx.inject(KEY, proc(scoped: std::service::Context) { ... })` | Admitted dependent setup |
| `ctx.on_dispose(proc() { ... })` | Register owned cleanup work |
| `registration.dispose()` | Withdraw provider and clean up admitted dependents |
| `ctx.dispose()` | Dispose children, provisions and cleanup work |

A key denotes its linked source declaration, not just its diagnostic label.
Two declarations with the same label remain different keys. Isolation affects
one key; unrelated keys keep their resolutions. A provider in an isolation is
visible to root access in that isolation regardless of which derived context
owns its registration. Injected views instead use their admitted bindings.

Service references remain live: withdrawing a provider makes new uses fail;
reproviding permits ordinary root references to see the new binding. Injected
cleanup retains its old admitted binding, even if a replacement is installed.
Dependents clean up before their provider. In-flight methods retain their receiver
through reentrant withdrawal, input suspension, budget pauses, and GC. Once no
admission, cleanup, or call needs the retired provider, its runtime root is released.
Disposal is idempotent. Failed cleanup stops execution with the normal source
error and preserves state for inspection.

Setup and cleanup closures must own their captures. A borrowed closure cannot
silently become a long-lived cleanup/admission callback. Arbitrary Cordis proxy
accessor/mixin/tracing protocols and asynchronous plugin scheduling are not part
of this initial runtime; effectful extension getters provide explicit lookup work.

## Stable slots and bounded native specialization

Each context view has a dense table indexed by linked key IDs. Provision and
withdrawal update only the changed key's routes; ordinary calls do no string
hashing or parent traversal. A cold registry index maintains active providers by
numeric key/isolation. Routine returns avoid registry scans when nothing retires.

Winch and Cranelift construct service references natively and read scalar service
handles directly. Native trait-call sites load the current binding and guard
up to four cached implementation targets. A hit skips method-table lookup and
uses the target's stable native entry slot. Frame construction, admission, GC
publication, and misses retain runtime helpers. An uncached implementation takes
the checked fallback; the cache does not grow beyond four targets.

The target guard depends on implementation identity, while the receiver is
reloaded from the current binding. Same-implementation replacement reuses the
case. Withdrawal and unrelated-service changes do not require recompiling or
fully deoptimizing callers. These are bounded per-site target cases; speculative
method inlining and provider constant-folding are deliberately deferred.

Caches and native resolution mirrors belong to a Session. Compiler workers use
immutable Program metadata; native installation uses the existing execution-thread
entry slots. REPL source replacement remaps existing function/key identities and
resets target caches. Moving GC repairs trait receivers and refreshes native views.

`Session::service_stats()` and `--jit-trace=summary` expose slot hits, route resolutions,
target hits/misses, native target hits, maximum target cases (library API),
provisions, withdrawals, and released bindings. Tests cover full native execution
and one-instruction engine handoffs with GC stress, replacement, isolation,
cleanup/reentrancy, private imports, constant cycles, and source mapping.
