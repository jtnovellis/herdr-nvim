-- Filtering is debounced. Typing a character and deleting it again inside the
-- debounce window must leave the list showing the *current* prompt, not the
-- character that is no longer there: an early-return on "same as the applied
-- query" left the already-scheduled timer to apply the stale one.
local P = require("herdr-nvim.picker")

local cands = {
  { path = "/repo/src/alpha.rs", session = true },
  { path = "/repo/src/bravo.rs", session = true },
  { path = "/repo/src/charlie.rs", session = true },
}

P.open({ candidates = vim.deepcopy(cands), cwd = "/repo", max_files = 20 }, { force_without_ui = true })
check(P.is_open(), "picker did not open")

local prompt_buf, list_buf
for _, w in ipairs(vim.api.nvim_list_wins()) do
  local cfg = vim.api.nvim_win_get_config(w)
  if cfg.relative == "editor" then
    if cfg.height == 1 then
      prompt_buf = vim.api.nvim_win_get_buf(w)
    else
      list_buf = vim.api.nvim_win_get_buf(w)
    end
  end
end
check(prompt_buf and list_buf, "could not locate the picker floats")

local function lines()
  return table.concat(vim.api.nvim_buf_get_lines(list_buf, 0, -1, false), "\n")
end
local function type_query(q)
  vim.api.nvim_buf_set_lines(prompt_buf, 0, 1, false, { "› " .. q })
  vim.api.nvim_exec_autocmds("TextChanged", { buffer = prompt_buf })
end

local all = lines()
check(all:find("alpha") and all:find("charlie"), "expected all three candidates initially: " .. all)

-- Type "alpha", then delete it again well inside the 30 ms debounce window.
type_query("alpha")
type_query("")

-- Let every scheduled timer fire.
vim.wait(300, function()
  return false
end, 10)

local after = lines()
check(after:find("charlie") ~= nil, "the debounce applied a query that had already been deleted: " .. after)
check(after:find("alpha") ~= nil, "lost candidates entirely: " .. after)

-- And a query that is actually typed still takes effect.
type_query("charlie")
vim.wait(500, function()
  return lines():find("alpha") == nil
end, 10)
local filtered = lines()
check(filtered:find("charlie") ~= nil, "typed query did not apply: " .. filtered)
check(filtered:find("alpha") == nil, "typed query did not filter: " .. filtered)
P.close()
pass("picker debounce applies the current query, not a deleted one")
