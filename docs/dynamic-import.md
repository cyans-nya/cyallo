# Runtime source loading

`std::module::load(path)` loads a new `.ya` file at runtime and returns
`Result<dynamic, std::module::LoadError>`. Its successful value is a readonly
[general object](objects.md) containing the module's public exports.
`use` remains a lexical import; `pub` and `pub use` control exports.

```cyallo
use std::module::load
let module = load("plugin.ya").unwrap()
let export_name = "twice"
std::io::print(module[export_name](21))
```

The file can contain ordinary declarations:

```cyallo
pub fn twice(value: i64) -> i64 { return value * 2 }
pub const label = "runtime"
pub mod tools { pub const version = 1 }
pub record Data { value: i64 }
```

`module.tools.version` accesses a child namespace. `module.Data` is an opaque
runtime type descriptor; it does not introduce a static type name in the caller.
Private declarations are absent. Colliding public export names cause a typed
loading error. The file need not exist during initial compilation.

Properties, computed keys, calls and operators use the same operations as other
dynamic values. Native reexports and source generics specialize through ordinary
declaration/type resolution, using actual argument types. Source generics whose
parameters cannot be inferred from inputs need specialized source wrappers.

## Optional structural views

A view checks an open interface and retains the same namespace and extra exports:

```cyallo
type Plugin = object { twice: fn(i64) -> i64, .. }
let plugin = std::object::view<Plugin>(module).unwrap()
std::io::print(plugin.twice(21), ":", plugin.label)
```

Missing or incompatible required fields return `ViewError`. Generic/native
callable fields instantiate against the requested signature. Views do not
project nominal records. Namespace writes and deletions fail through every alias
and view. Aggregate constants are read as owned copies; changing a checked local
array does not change module constants.

Loaded files may declare `mod` children. Their `crate::` refers to that loaded
package; `super::` stops at its root. They cannot access the caller's private
namespace. Initialization permits constants; effects belong in explicit
procedures. Loading never replays the caller's top-level code.

Run [the example](../examples/modules/dynamic/main.yas):

```sh
cargo run -- run examples/modules/dynamic/main.yas --engine cranelift_speed --gc-stress
```

It prints `runtime`, `42`, `42`, and `true` on separate lines. The
[coroutine example](../examples/concurrency/modules/main.yas) prints `42`.

## Authority and ownership

Paths are relative to the source file containing `load`. The CLI and
`compile_path` grant the entry file's canonical directory. All descendants must
remain inside that explicit grant. Leaf symlinks, traversal outside the grant,
ambiguous sources and static module cycles are rejected by the shared loader.
In-memory compilation and the REPL have no ambient read grant; an embedder can
attach an explicit `ModuleLoader` with `Program::with_module_loader`.

Each package inherits the caller's standard-module permission ceiling.
Defining-module checks cover aliases, reexports, native members and dynamically
specialized calls. The API comes from [std/module.ya](../std/module.ya), with
private semantic ABI annotations rather than a catalog of public paths.

Loaded functions execute in the caller's coroutine and heap. Local `gc`/`rc`
values can pass through checked parameters. Loading neither transports them to
another task nor promotes them to shared storage. Object namespaces stay local.

Canonical file identity and inherited permissions select a retained snapshot
within the coroutine's program lineage. Repeated loads return the same namespace
object, even if its source changes or disappears. Independent coroutines and
restarted Sessions initialize their own namespaces. Loading a known path does
not reload changed source.

## Suspension, publication and errors

Loading and dynamic callable specialization use owned jobs on the bounded host
pool. The C saves its destination and successor PC before submission, parks
without a P, then resumes once. Heap arguments stay in coroutine-owned,
relocatable roots; host workers receive code snapshots, types and declarations.
Cancellation prevents late completions from publishing into a cancelled C.

Checking retains [module products](modules.md) and can reuse their checked IR.
Publication invokes only newly added module initializers; imported source bodies
are never merged into the entry AST.

Publication appends code and globals, preserving routine IDs, PCs, frame layouts,
roots and suspended generators. Native drivers exit before publication and
continue the extended snapshot in the interpreter. Native compilation of newly
loaded code remains future work; unrelated artifacts stay rejected.

`LoadError` retains loader, checking, ambiguous-export, publication, host and
cancellation causes. Its methods are `is_not_found()`, `is_denied()` (file read
authority), `is_check_error()` and `message()`. A denied standard import is a
checking error. These are ordinary Result values. Dynamic property/call failures
retain typed runtime causes; structural checks return `ViewError`.

[Language tests](../tests/dynamic_import.rs) cover new files, namespace isolation,
permissions, caching, restart, managed arguments, generators and native fallback.
[Object tests](../tests/objects.rs) exercise the shared machinery with GC stress
and single-instruction handoffs.
