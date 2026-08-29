-- matchfuzzypos() reports character indices; extmark columns are byte offsets.
-- For a path containing a multibyte character the two diverge, so the match
-- highlight used to land on the wrong column -- or on a mid-codepoint column,
-- which throws and is swallowed by the surrounding pcall, silently dropping
-- the highlight altogether.
local P = require("herdr-nvim.picker")

local dir = tmp("e\xcc\x81dir") -- "e" + combining acute: multibyte, still a valid dir name
vim.fn.mkdir(dir, "p")
local path = dir .. "/abc.lua"
vim.fn.writefile({ "-- fixture" }, path)

P.open({ candidates = { { path = path } }, cwd = TMP, max_files = 20 }, { force_without_ui = true })
check(P.is_open(), "picker did not open")

-- The two floats differ by height: the prompt is a single line.
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
check(prompt_buf and list_buf, "could not locate the prompt and list floats")

-- Type a query matching the two ASCII characters that follow the multibyte one.
vim.api.nvim_buf_set_lines(prompt_buf, 0, 1, false, { "› ab" })
vim.api.nvim_exec_autocmds("TextChanged", { buffer = prompt_buf })

-- Filtering is debounced, so wait for the list to catch up.
local line
vim.wait(1000, function()
  line = vim.api.nvim_buf_get_lines(list_buf, 0, 1, false)[1]
  return line ~= nil and line:find("abc%.lua") ~= nil
end, 10)
check(line and line:find("abc%.lua"), "expected the candidate on line 1, got " .. tostring(line))

local ns = vim.api.nvim_get_namespaces()["herdr-nvim-picker"]
check(ns ~= nil, "picker namespace missing: " .. vim.inspect(vim.tbl_keys(vim.api.nvim_get_namespaces())))

local highlighted = {}
for _, mk in ipairs(vim.api.nvim_buf_get_extmarks(list_buf, ns, 0, -1, { details = true })) do
  local row, col, det = mk[2], mk[3], mk[4]
  if det.hl_group == "HerdrNvimPickerMatch" and row == 0 then
    table.insert(highlighted, line:sub(col + 1, det.end_col))
  end
end
table.sort(highlighted)
check(#highlighted == 2, "expected 2 match highlights, got " .. #highlighted .. ": " .. vim.inspect(highlighted))
check(
  highlighted[1] == "a" and highlighted[2] == "b",
  "highlights landed on the wrong bytes: " .. vim.inspect(highlighted)
)
P.close()
pass("picker highlights multibyte paths on the right bytes")
