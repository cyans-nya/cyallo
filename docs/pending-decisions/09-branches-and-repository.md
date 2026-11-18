# 09 — Branches and repository state

- `main` (created from `data-enums`, commit `d6ebc04`) and `overhaul` (commit `62aa665`) are
  separate lines from `f51c909`. `overhaul` carries the language server, tree-sitter grammars,
  editor integrations and module graph work; `main` carries the data-enum work. Both change
  many of the same files, so merging will conflict.
- `data-enums` and `adaptive-native` branches still exist; the `data-enums` worktree is in
  `.worktrees/data-enums`.
- Nothing has been pushed. The remote `github` has only `overhaul`.
- Untracked: `nvim.log`, `.worktrees/` (not ignored).

**Decision needed.** Merge order and target (merge `overhaul` into `main`, or the reverse),
whether to push `main`, and whether `.worktrees/` and `nvim.log` should be git-ignored.
