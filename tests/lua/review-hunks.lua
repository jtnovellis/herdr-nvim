-- What the agent edited, marked where it landed, navigable, and revertible.
local R = require("herdr-nvim.review")

edit("hn.txt") -- a\nb\nc\nd\ne
local buf = vim.api.nvim_get_current_buf()

-- The agent replaced "c" with "see". Its write already reached the buffer,
-- which is the whole point: the hunk describes something already applied.
vim.api.nvim_buf_set_lines(buf, 2, 3, false, { "see" })
R.record({ { path = vim.api.nvim_buf_get_name(buf), old = "c", new = "see" } }, "claude")

local hunks = R.list(buf)
check(#hunks == 1, "expected one hunk, got " .. #hunks)
check(hunks[1].srow == 2, "hunk on the wrong row: " .. hunks[1].srow)
check(R.count() == 1, "count disagrees with list")
check(require("herdr-nvim").statusline():find("~1", 1, true) ~= nil, "hunk not in statusline")

-- An edit whose text is not in the buffer is held, not placed: a later
-- reload may still bring it in.
R.record({ { path = vim.api.nvim_buf_get_name(buf), old = "zz", new = "nowhere" } }, "claude")
check(R.count() == 1, "an absent edit was placed anyway")

-- Navigation lands on the hunk.
vim.api.nvim_win_set_cursor(0, { 1, 0 })
local target = R.jump(1)
check(target ~= nil, "jump found nothing")
check(vim.api.nvim_win_get_cursor(0)[1] == 3, "jump went to the wrong line")

-- Revert puts the file back the way it was and drops the mark.
local item = R.find_at(buf, 2)
check(item ~= nil, "find_at missed the hunk")
local ok, err = R.revert(item)
check(ok, "revert failed: " .. tostring(err))
check(vim.api.nvim_buf_get_lines(buf, 2, 3, false)[1] == "c", "revert did not restore the text")
check(R.count() == 0, "mark survived the revert")

-- Reverting a hunk whose text has since changed must refuse rather than
-- clobber whatever replaced it.
vim.api.nvim_buf_set_lines(buf, 3, 4, false, { "dee" })
R.record({ { path = vim.api.nvim_buf_get_name(buf), old = "d", new = "dee" } }, "claude")
local moved = R.find_at(buf, 3)
check(moved ~= nil, "second hunk not placed")
-- Edited *inside* the marked range, so the extmark survives and still points
-- at text that is no longer what the agent wrote. This is the dangerous case:
-- a blind revert here would overwrite a human's edit.
vim.api.nvim_buf_set_text(buf, 3, 1, 3, 2, { "X" })
local ok2, err2 = R.revert(moved)
check(not ok2, "revert clobbered text the agent did not write")
check(tostring(err2):find("changed", 1, true) ~= nil, "unhelpful refusal: " .. tostring(err2))

-- A whole-file write has no "before", so it is marked but cannot be reverted.
R.clear(buf)
R.record({ { path = vim.api.nvim_buf_get_name(buf), old = nil, new = "whatever" } }, "claude")
local written = R.list(buf)
check(#written == 1, "a write was not marked")
local ok3, err3 = R.revert(written[1])
check(not ok3 and tostring(err3):find("created", 1, true) ~= nil, "write revert: " .. tostring(err3))

-- The agent records the path it used, which need not be the spelling Neovim
-- opened the buffer under: on macOS `/tmp` is `/private/tmp` and `/var` is
-- `/private/var`, so comparing the two as strings finds nothing. A redundant
-- `/./` segment reproduces that mismatch on every platform -- a symlink does
-- not, because Neovim resolves one at open time on Linux but not on macOS.
R.clear()
local real = tmp("aliased.txt")
local f = assert(io.open(real, "w"))
f:write("one\ntwo\nthree\n")
f:close()
local aliased = TMP .. "/./aliased.txt"
check(aliased ~= real, "the two spellings coincided; the test proves nothing")

vim.cmd("edit " .. vim.fn.fnameescape(real))
local abuf = vim.api.nvim_get_current_buf()
R.record({ { path = aliased, old = "two", new = "two" } }, "claude")
check(#R.list(abuf) == 1, "an edit recorded under the other spelling was lost")
R.clear()

pass("review hunks")
