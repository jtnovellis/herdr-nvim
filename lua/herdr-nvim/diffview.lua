-- Resolve the displayed revision from Diffview's file model, not its abbreviated URI.
local M = {}

function M.current(buf)
  local lib = package.loaded["diffview.lib"]
  local view = lib and lib.get_current_view()
  local layout = view and view.cur_layout
  if not layout or layout.c or layout.d then
    return nil
  end

  local types = require("diffview.vcs.rev").RevType
  for _, entry in ipairs({ { "a", "left" }, { "b", "right" } }) do
    local win = layout[entry[1]]
    local file = win and win.file
    if win and win.id == vim.api.nvim_get_current_win() and file and file.bufnr == buf then
      if file.nulled or file.binary or not file.rev or not file.adapter.ctx.toplevel then
        return nil
      end
      local rev = file.rev
      local revision
      if rev.type == types.COMMIT then
        revision = rev.commit
      elseif rev.type == types.LOCAL then
        revision = "WORKTREE"
      elseif rev.type == types.STAGE then
        revision = "INDEX:" .. rev.stage
      end
      if not revision then
        return nil
      end
      return {
        file = file.absolute_path,
        diff = {
          repo_root = file.adapter.ctx.toplevel,
          path = file.path,
          revision = revision,
          side = entry[2],
        },
      }
    end
  end
end

return M
