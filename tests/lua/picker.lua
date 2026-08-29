edit("hn.txt")
local P = require("herdr-nvim.picker")

local cands = {
  { path = tmp("other.txt"), line = 3, session = true, touched_unix = os.time() - 120, diff_stat = { 2, 1 } },
  { path = tmp("hn.txt"), session = true, newly_created = true },
  { path = "/nowhere/repo/deep/target.rs", session = false },
}
for _, c in ipairs(cands) do
  c.display = c.path
end

local m = P.rank(cands, "", 1)
check(#m == 1 and m[1].cand.path == tmp("other.txt"), "empty query caps to max_files and keeps session rows")

local m2 = P.rank(cands, "target", 20)
check(
  #m2 >= 1 and m2[1].cand.path == "/nowhere/repo/deep/target.rs",
  "typed query reaches repo-only rows: " .. vim.inspect(m2)
)

P.open({ candidates = vim.deepcopy(cands), cwd = TMP, max_files = 20 }, { force_without_ui = true })
check(P.is_open() == true, "picker did not open")

local floats = 0
for _, w in ipairs(vim.api.nvim_list_wins()) do
  if vim.api.nvim_win_get_config(w).relative == "editor" then
    floats = floats + 1
  end
end
check(floats == 2, "expected prompt + list floats, got " .. floats)

vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<CR>", true, false, true), "x", false)
check(not P.is_open(), "picker still open after <CR>")
check(
  vim.fn.expand("%:t") == "other.txt" and vim.fn.line(".") == 3,
  "did not open other.txt:3, got " .. vim.fn.expand("%:t") .. ":" .. vim.fn.line(".")
)

-- handoff file with JSON nulls, deferred until a UI attaches
local f = tmp("handoff.json")
vim.fn.writefile({
  vim.json.encode({
    candidates = { { path = tmp("hn.txt"), line = vim.NIL, session = true, diff_stat = vim.NIL } },
    cwd = TMP,
    max_files = 20,
  }),
}, f)
check(P.open_file(f) == true, "open_file failed")
check(vim.fn.filereadable(f) == 0, "handoff not deleted")
check(not P.is_open(), "picker opened without a UI")

vim.api.nvim_exec_autocmds("UIEnter", {})
vim.wait(200, function()
  return P.is_open()
end, 10)
check(P.is_open(), "picker did not open on UIEnter")
P.close()
pass("picker")
