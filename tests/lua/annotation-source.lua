local has_diffview = pcall(require, "diffview.lib")
check(not has_diffview, "this test must run without Diffview installed on runtimepath")

local H = require("herdr-nvim")
local A = require("herdr-nvim.annotations")
H.config.notify = false
vim.notify = function() end
vim.ui.input = function(_, cb)
  cb("note")
end

edit("hn.txt")
H.annotate({ line1 = 2, line2 = 3 })
local item = A.list()[1]
check(item.file == vim.api.nvim_buf_get_name(0), "normal annotation lost its file")
check(item.diff == nil and item.row == 1 and item.end_row == 2, "normal annotation changed")
check(package.loaded["diffview.lib"] == nil, "normal annotation loaded Diffview")
A.clear()

for _, buftype in ipairs({ "nofile", "nowrite", "prompt" }) do
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_set_current_buf(buf)
  vim.api.nvim_buf_set_name(buf, "arbitrary-" .. buftype)
  vim.bo[buf].buftype = buftype
  H.annotate()
  check(A.total() == 0, "accepted arbitrary " .. buftype .. " buffer")
end

local buf = vim.api.nvim_create_buf(false, false)
vim.api.nvim_set_current_buf(buf)
vim.api.nvim_buf_set_name(buf, "diffview://unrelated/fake.lua")
H.annotate()
check(A.total() == 0, "accepted a Diffview URI without a live file model")
pass("annotation source validation")
