# Demo: one project that should work

Part of the [component worlds draft](README.md). Status: proposal, 2026-11-23.
Part A runs today in the `ir-pipeline` worktree (checked with `cyallo run`). Parts B
and C need `std::module` (`Source`, `Manifest`, `Bindings`, `load`), which is not
implemented. Where the target syntax differs from today's, the demo uses today's.
For example, the drafts write `read path: string`, but the parser currently rejects
`read` (E173), so the demo writes `path: string`.

Sources are loaders with an explicit entry (`lib.ya` below is just the name these
plugins chose). `Source::read(dir)` reads a directory into memory; an in-memory
source is `Source::new().path("lib.ya", text)`. See the
[revision](README.md#revision-2026-11-23) for the `Loader` trait, lazy loading and
packed `.yamod` sources.

## Layout

```text
demo/
  cyallo.toml                  # root package (name, version); no imports
  main.ya                      # entry: declares proc main()
  report.ya                    # module `report`: a file is a module, no `mod` line
  net/
    mod.ya                     # module `net`; gates its children
    client.ya                  # exported by `pub use client`
    secret.ya                  # not exported: private to `net`
  data/ok.txt                  # a file the sandboxed plugin may read
  scripts/tour.yas             # a script
  plugins/
    summarize/{cyallo.toml,lib.ya}   # pure plugin
    shout/{cyallo.toml,lib.ya}       # imports component `text`
    snoop/{cyallo.toml,lib.ya}       # wants std::fs, will not get it
    probe/{cyallo.toml,lib.ya}       # gets a sandboxed std::fs
  vendor/text/{cyallo.toml,lib.ya}   # a dependency the host chooses to supply
```

## A. Files are modules (runs today)

```cyallo
// main.ya: a `.ya` entry. Top level holds declarations, `const` and immutable `let`.
use crate::net::client::ping
use crate::report::banner

const NAME = "demo"
let LIMIT = 100                  // immutable `let` is a compile-time constant here

fn clamp(n: i64) -> i64 {
    if n > LIMIT { return LIMIT }
    return n
}

proc main() {
    std::io::print(banner(NAME))     // DEMO
    std::io::print(ping(21))         // 42
    std::io::print(clamp(250))       // 100
}
```

```cyallo
// report.ya
pub fn banner(name: string) -> string {
    return std::string::upper(name)
}
```

```cyallo
// net/mod.ya: the gatekeeper. Children are private to `net` unless re-exported.
pub use client
```

```cyallo
// net/client.ya
use super::secret::token         // a sibling is visible inside `net`

pub fn ping(n: i64) -> i64 {
    return n + token()
}
```

```cyallo
// net/secret.ya
pub fn token() -> i64 {
    return 21
}
```

```text
$ cyallo run demo/main.ya
DEMO
42
100
```

A script is a separate mode. It never looks for `main`, and `.ya` never falls back
to script mode:

```cyallo
// scripts/tour.yas
fn square(n: i64) -> i64 { return n * n }
std::io::print(square(7))
```

```text
$ cyallo run demo/scripts/tour.yas
49
```

These are rejected, and the first three were checked today:

| Source | Result |
| --- | --- |
| `use crate::net::secret::token` in `main.ya` | `E140 unresolved or private import`: `net/mod.ya` does not export `secret` |
| `std::io::print(1)` at the top level of a `.ya` file | `E140 top level allows declarations, const and immutable let; put statements in proc main()` |
| a `.ya` entry with no `proc main()` | `E140 the entry file must declare proc main()` |
| `net.ya` beside `net/mod.ya` | error: two sources claim `net` (not implemented yet) |

## B. A host loads components (target)

Everything a component can reach is what its host binds. The host decides where
sources come from and calls one primitive, `std::module::load(source.entry("lib.ya"), bindings)`.
These are `proc` because they read files.

```cyallo
// main.ya (continued)

// A plugin with no authority: pure std only.
proc load_pure(dir: string) -> Result<std::module::Instance, std::module::LoadError> {
    let source = std::module::Source::read(dir)?          // uses this component's std::fs
    return std::module::load(source.entry("lib.ya"), std::module::Bindings::none())
}

// Where a dependency comes from is host policy. This host only trusts `vendor/`.
proc locate(name: string) -> Result<std::module::Source, std::module::LoadError> {
    if name == "text" { return std::module::Source::read("vendor/text") }
    return Err(std::module::LoadError::NotFound { path: name })
}

// A plugin whose manifest asks for components: read the manifest, supply each one.
proc load_with_dependencies(dir: string) -> Result<std::module::Instance, std::module::LoadError> {
    let source = std::module::Source::read(dir)?
    let manifest = source.manifest()?                     // parses its cyallo.toml
    let mut bindings = std::module::Bindings::none()
    for dep in manifest.imports {
        let found = locate(dep.name)?
        if !dep.range.satisfies(found.manifest()?.version) {
            return Err(std::module::LoadError::MissingComponent {
                name: dep.name, range: dep.range.text(),
            })
        }
        // A component import is one more binding: this instance, shared by name.
        bindings = bindings.bind(dep.name, std::module::load(found.entry("lib.ya"), std::module::Bindings::none())?)
    }
    return std::module::load(source.entry("lib.ya"), bindings)
}
```

```toml
# plugins/summarize/cyallo.toml
[package]
name = "summarize"
version = "0.1.0"
```

```cyallo
// plugins/summarize/lib.ya
pub fn twice(n: i64) -> i64 { return n * 2 }
```

```toml
# plugins/shout/cyallo.toml
[package]
name = "shout"
version = "0.1.0"

[imports]
text = "^1"
```

```cyallo
// plugins/shout/lib.ya: `text` is reachable as a root path because the host bound it
pub fn shout(s: string) -> string { return text::loud(s) }
```

```cyallo
// vendor/text/lib.ya   (cyallo.toml: name = "text", version = "1.2.0")
pub fn loud(s: string) -> string { return std::string::upper(s) }
```

```cyallo
// main.ya (continued)
proc show_twice() {
    match load_pure("plugins/summarize") {
        Ok(plugin) => std::io::print(plugin.namespace().twice(42)),   // 84
        Err(error) => std::io::print(error.message()),
    }
}

proc show_shout() {
    match load_with_dependencies("plugins/shout") {
        Ok(plugin) => std::io::print(plugin.namespace().shout("hello")),   // HELLO
        Err(error) => std::io::print(error.message()),
    }
}
```

### Sources built in memory or packed

A component can have many files, and its source needs no filesystem. `helper.ya` is
module `helper` because every file is a module, and `lib.ya` is the entry only
because this host named it. `pack` and `unpack` round-trip the same tree through a
`.yamod`.

```cyallo
proc load_inline() -> Result<std::module::Instance, std::module::LoadError> {
    let source = std::module::Source::new()
        .path("lib.ya", "use crate::helper::double\npub fn quad(n: i64) -> i64 { return double(double(n)) }\n")
        .path("helper.ya", "pub fn double(n: i64) -> i64 { return n * 2 }\n")
    let packed = std::module::Source::unpack(source.pack())?   // pure and bounded: no std::fs
    return std::module::load(packed.entry("lib.ya"), std::module::Bindings::none())
}

proc show_inline() {
    match load_inline() {
        Ok(plugin) => std::io::print(plugin.namespace().quad(5)),   // 20
        Err(error) => std::io::print(error.message()),
    }
}
```

## C. Least privilege and uniform not-found (target)

```cyallo
// plugins/snoop/lib.ya: needs std::fs
pub proc peek() -> bool {
    match std::fs::read_to_string("main.ya") {
        Ok(_) => return true,
        Err(_) => return false,
    }
}
```

```cyallo
// plugins/probe/lib.ya: the same, for any path
pub proc exists(path: string) -> bool {
    match std::fs::read_to_string(path) {
        Ok(_) => return true,
        Err(_) => return false,
    }
}
```

```cyallo
// main.ya (continued)

// A wrapper module is the attenuation. It maps "outside data/" and "absent" to the
// same error, so the plugin cannot tell them apart.
mod sandbox_fs {
    pub proc read_to_string(path: string) -> Result<string, std::fs::Error> {
        if !std::string::starts_with(path, "data/") {
            return Err(std::fs::Error::not_found(path))
        }
        match std::fs::read_to_string(path) {
            Ok(text) => return Ok(text),
            Err(_) => return Err(std::fs::Error::not_found(path)),
        }
    }
}

proc show_snoop() {
    // No binding for std::fs: the load fails, naming the unprovided path.
    match load_pure("plugins/snoop") {
        Ok(_) => std::io::print("snoop loaded"),
        Err(_) => std::io::print("snoop rejected"),             // snoop rejected
    }
}

proc show_probes() {
    let source = std::module::Source::read("plugins/probe")
    // `?` is not available here: this proc returns nothing, so match the result.
    match source {
        Ok(source) => {
            let bindings = std::module::Bindings::none().bind("std::fs", crate::sandbox_fs)
            match std::module::load(source.entry("lib.ya"), bindings) {
                Ok(probe) => show_exists(probe.namespace()),
                Err(error) => std::io::print(error.message()),
            }
        }
        Err(error) => std::io::print(error.message()),
    }
}

proc show_exists(api: dynamic) {
    std::io::print(api.exists("data/ok.txt"))       // true
    std::io::print(api.exists("data/absent.txt"))   // false: absent
    std::io::print(api.exists("main.ya"))           // false: exists, but outside the scope
}
```

Expected output of the whole `main`, with `proc main()` calling `show_twice()`,
`show_shout()`, `show_inline()`, `show_snoop()` and `show_probes()` after the part A lines:

```text
DEMO
42
100
84
HELLO
20
snoop rejected
true
false
false
```

Must be rejected:

| Source | Result |
| --- | --- |
| `.bind("std::net", std::net)` in a component whose own world lacks `std::net` | compile error in the host: it cannot name what it does not hold, so authority cannot grow |
| a child using `std::fs` that was not bound | `LoadError::Check`: `std::fs::read_to_string` is not provided. The text is the same as for a misspelled path |
| `Source::read` of a path outside the host's `std::fs` scope | the same not-found as an absent path, with the cause visible only to the host |
| a `text` import with no `vendor/text` | `LoadError::MissingComponent`, raised by the host's own `locate` before the child is parsed |

## Findings from checking part A

- **Nested packages are swept in.** The current disk scan loads every `.ya` file
  below the entry's directory. A test directory with its own `main.ya` inside
  `demo/` was loaded as a module of `demo` and its error surfaced. The scan should
  stop at any subdirectory with its own `cyallo.toml`, as
  [01](01-paths-and-values.md#packages) already says. With that rule `plugins/` and
  `vendor/` stay out of `demo`'s module tree, which this demo depends on.
- **Drafts conflict with this session's decisions.**
  [01](01-paths-and-values.md#proposed-changes) collapses visibility to `pub` versus
  anywhere-in-package and keeps `mod foo;` for single-file packages. The demo uses
  Rust-style module privacy with the `mod.ya` gate, and files-as-modules.
  [04](04-versioning-and-loading.md#errors) has `LoadError::Denied` next to
  `NotFound`, which conflicts with uniform not-found.
- **Assumed std additions.** `Source::read` (best implemented over the caller's
  bound `std::fs` members, so wrappers apply), `Source::manifest`,
  `Manifest.imports` and `.version`, `Range::satisfies` and `Range::text`,
  `std::fs::Error::not_found`, `Source::new().path(...)`, `Source::pack` and `unpack`,
  and a `LoadError::Manifest` case for `?` to use. None exist yet.
