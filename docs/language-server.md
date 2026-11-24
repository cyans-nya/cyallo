# Cyallo language server

`cya-ls` provides syntax highlighting through LSP semantic tokens. The server,
Zed, and Neovim share the same Tree-sitter grammars and highlight queries.
Cyallo modules (`.ya`) and scripts (`.yas`) use the `cyallo` language ID.
CAIE pseudocode (`.psc` and `.pss`) uses `caie`.

From the repository root, run `cargo build --locked -p cya-ls` to build
`target/debug/cya-ls`. Use `cargo install --path crates/cya-ls --locked` to put
the server on your Cargo bin path. Both editors launch it with `--stdio`.

## Zed

Run `cargo xtask editors zed-dev` to generate the development extension with
the current grammars and queries. In Zed, open **Extensions**, choose
**Install Dev Extension**, and select `target/editor-dev/zed`. If the extension
is already installed, rebuild it from the Extensions panel after regenerating
the package. Zed builds the extension for `wasm32-wasip2` and uses the WASI SDK
to compile the grammars. [Zed extension development](https://zed.dev/docs/extensions/developing-extensions)

The launcher checks `lsp.cya-ls.binary.path`, then the worktree's shell `PATH`,
then the checkout's `target/debug/cya-ls`. Open a supported file and check that
Zed selects **Cyallo** or **CAIE Pseudocode**. To see LSP token colors alongside
Tree-sitter highlighting, add these per-language settings. [Zed semantic tokens](https://zed.dev/docs/semantic-tokens)

```json
{
  "languages": {
    "Cyallo": { "semantic_tokens": "combined" },
    "CAIE Pseudocode": { "semantic_tokens": "combined" }
  }
}
```

## Neovim

Use Neovim 0.11 or newer. Add the checkout's runtime to your configuration.

```lua
local repo = '/absolute/path/to/cyallo'
vim.opt.runtimepath:prepend(repo .. '/editors/neovim')
require('cyallo').setup({ cmd = { repo .. '/target/debug/cya-ls', '--stdio' } })
```

With `setup()` alone, Neovim looks for `cya-ls` on `PATH`. For Tree-sitter
highlighting, run `:lua require('cyallo').install_parsers()` with a C compiler
available. Run it again after updating the grammars, then restart Neovim to
load the rebuilt parsers. Pass `lsp = false` to `setup` for Tree-sitter
highlighting alone.

## Verification

```sh
cargo test --locked -p cya-ls
cargo xtask editors generate --check
cargo check --locked --manifest-path target/editor-dev/zed/Cargo.toml --target wasm32-wasip2
```

`cargo xtask editors generate` regenerates the C parsers with Tree-sitter
CLI 0.27.2. Set `TREE_SITTER_CLI` to a local copy of that version if available.

Run the headless Neovim smoke test against the built server and an isolated
data directory.

```sh
mkdir -p target/neovim-test
XDG_DATA_HOME="$PWD/target/neovim-test/data" \
XDG_STATE_HOME="$PWD/target/neovim-test/state" \
XDG_CACHE_HOME="$PWD/target/neovim-test/cache" \
NVIM_LOG_FILE="$PWD/target/neovim-test/nvim.log" \
  nvim --headless -u NONE -i NONE -l editors/neovim/tests/smoke.lua
```
