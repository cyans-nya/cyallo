local M = {}
local root = vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(debug.getinfo(1, 'S').source:sub(2))))))

---Build the checked-in C parsers; requires a C compiler, not Node or the CLI.
function M.install_parsers()
  local destination = vim.fs.joinpath(vim.fn.stdpath('data'), 'site', 'parser')
  vim.fn.mkdir(destination, 'p')
  for _, language in ipairs({ 'cyallo', 'caie' }) do
    local grammar = vim.fs.joinpath(root, 'crates', 'tree-sitter-' .. language)
    local output = vim.fs.joinpath(destination, language .. '.so')
    local command = { vim.env.CC or 'cc', '-O2', '-shared', '-fPIC', '-I' .. grammar .. '/src', grammar .. '/src/parser.c', '-o', output }
    local result = vim.system(command, { text = true }):wait()
    assert(result.code == 0, 'Failed to build ' .. language .. ': ' .. (result.stderr or ''))
    vim.treesitter.language.add(language, { path = output })
  end
end

---Register baseline Tree-sitter highlighting and optionally cya-ls.
---@param options? { lsp?: boolean, cmd?: string[] }
function M.setup(options)
  options = options or {}
  vim.filetype.add({ extension = { ya = 'cyallo', yas = 'cyallo', psc = 'caie', pss = 'caie' } })
  for _, language in ipairs({ 'cyallo', 'caie' }) do
    local query = vim.fs.joinpath(root, 'crates', 'tree-sitter-' .. language, 'queries', 'highlights.scm')
    vim.treesitter.query.set(language, 'highlights', table.concat(vim.fn.readfile(query), '\n'))
    -- Theme fallbacks for captures shared with the Zed and LSP adapters.
    vim.api.nvim_set_hl(0, '@namespace.' .. language, { link = '@module', default = true })
    vim.api.nvim_set_hl(0, '@lifetime.' .. language, { link = '@type', default = true })
  end
  local group = vim.api.nvim_create_augroup('CyalloHighlighting', { clear = true })
  vim.api.nvim_create_autocmd('FileType', {
    group = group,
    pattern = { 'cyallo', 'caie' },
    callback = function(event)
      if not vim.treesitter.language.add(event.match) then
        return
      end
      local query_ok, query_error = pcall(vim.treesitter.query.get, event.match, 'highlights')
      if not query_ok then
        vim.notify_once('Rebuild Cyallo parsers with :lua require("cyallo").install_parsers() and restart Neovim.\n'
          .. tostring(query_error), vim.log.levels.WARN)
        return
      end
      vim.treesitter.start(event.buf, event.match)
    end,
  })
  if options.lsp ~= false then
    vim.lsp.config('cya_ls', {
      cmd = options.cmd or { 'cya-ls', '--stdio' },
      filetypes = { 'cyallo', 'caie' },
      root_markers = { '.git' },
    })
    vim.lsp.enable('cya_ls')
  else
    vim.lsp.enable('cya_ls', false)
  end
end

return M
