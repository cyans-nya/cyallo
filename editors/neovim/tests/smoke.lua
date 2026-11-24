local root = vim.fn.getcwd()
vim.opt.runtimepath:prepend(root .. '/editors/neovim')
local cyallo = require('cyallo')
cyallo.install_parsers()
cyallo.setup({ lsp = false })
vim.cmd('filetype on')

local fixtures = {
  { extension = 'ya', language = 'cyallo', text = { 'let value = 1', '// 水🐈' }, row = 0, column = 0 },
  {
    extension = 'yas',
    language = 'cyallo',
    text = { 'enum Option { Some: i64, None }', 'let n = match Option::Some(1) { Option::Some(n) => n, Option::None => 0 }' },
    row = 0,
    column = 0,
  },
  { extension = 'psc', language = 'caie', text = { 'dEcLaRe n : INTEGER', 'n ← 2', 'OUTPUT n' }, row = 0, column = 0 },
  { extension = 'pss', language = 'caie', text = { 'n <- 2', 'OUTPUT n' }, row = 1, column = 0 },
}
for _, fixture in ipairs(fixtures) do
  vim.cmd('enew!')
  vim.api.nvim_buf_set_name(0, root .. '/target/editor-smoke.' .. fixture.extension)
  vim.api.nvim_buf_set_lines(0, 0, -1, false, fixture.text)
  vim.cmd('filetype detect')
  assert(vim.bo.filetype == fixture.language, 'unexpected filetype for .' .. fixture.extension .. ': ' .. vim.bo.filetype)
  local tree = vim.treesitter.get_parser(0):parse()[1]
  assert(not tree:root():has_error(), tree:root():sexpr())
  assert(vim.treesitter.highlighter.active[vim.api.nvim_get_current_buf()])
  local captures = vim.treesitter.get_captures_at_pos(0, fixture.row, fixture.column)
  assert(vim.iter(captures):any(function(capture) return capture.capture == 'keyword' end), vim.inspect(captures))
  assert(#vim.lsp.get_clients({ bufnr = 0 }) == 0, 'baseline highlighting must work without LSP')
end

cyallo.setup({ cmd = { root .. '/target/debug/cya-ls', '--stdio' } })
for _, fixture in ipairs(fixtures) do
  vim.cmd('enew!')
  vim.api.nvim_buf_set_name(0, root .. '/target/editor-lsp-smoke.' .. fixture.extension)
  vim.api.nvim_buf_set_lines(0, 0, -1, false, fixture.text)
  vim.cmd('filetype detect')
  assert(vim.wait(5000, function()
    return #vim.lsp.get_clients({ bufnr = 0, name = 'cya_ls' }) > 0
  end), 'cya-ls did not attach for ' .. fixture.extension)
  local client = vim.lsp.get_clients({ bufnr = 0, name = 'cya_ls' })[1]
  assert(vim.wait(5000, function() return client.initialized end), 'cya-ls did not initialize')
  local result = client:request_sync('textDocument/semanticTokens/full', {
    textDocument = { uri = vim.uri_from_bufnr(0) },
  }, 5000, 0)
  assert(result and not result.err and #result.result.data > 0, vim.inspect(result))
end
for _, client in ipairs(vim.lsp.get_clients()) do client:stop(true) end
print('Cyallo/CAIE baseline highlighting and cya-ls attachment passed')
vim.cmd('qa!')
