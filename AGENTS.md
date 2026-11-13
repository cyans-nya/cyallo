# Compiler and runtime work

Before changing syntax, name/type resolution, builtin declarations, typed IR,
specialization, VM/JIT execution, concurrency, or error handling, read and follow
[compiler and runtime rules](docs/compiler-and-runtime-rules.md). These are project
requirements, including typed errors/statuses, declaration-based primitives, and
instance-owned runtime state.

For concurrency work, also read the [runtime contract](docs/concurrency.md) and
[language integration tracker](docs/concurrency-language-progress.md). Update the
tracker from verified language behavior; keep the feature's Cyallo examples in
`examples/concurrency/`.
Write language examples in Cyallo. Rust is for runtime/compiler implementation
and verification tests; do not add Rust example programs or external integration
work unless the user explicitly requests it.

# Python scripts

- Use `uv` to create, manage, and run Python environments and dependencies. Prefer
  standalone `uv` scripts with inline dependencies when appropriate.
- Use `typer` for command-line interfaces and `rich` for styled terminal output.

# Vite Plus and JavaScript tools

- Use `vp` for Vite Plus tasks and `vpx` for download-and-execute tasks. Add
  `~/.vite-plus/bin` to PATH when needed.
- Vite Plus supplies Node.js, pnpm, and yarn in that directory. Consult
  `~/.config/nyam/config.toml` for the installed Deno version and tool paths.
