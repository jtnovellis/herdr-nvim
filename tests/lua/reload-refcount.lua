-- Wiping one buffer must not tear down the directory watcher that another
-- still-open buffer in the same directory depends on. `:bwipeout` fires both
-- BufDelete and BufWipeout, which used to release the reference twice.
local R = require("herdr-nvim.reload")

edit("hn.txt")
local first = vim.api.nvim_get_current_buf()
edit("other.txt")
local second = vim.api.nvim_get_current_buf()
check(first ~= second, "expected two distinct buffers")

R.start({ debounce_ms = 50, force_without_ui = true })
check(R.status().watchers == 1, "expected one watcher for the shared directory, got " .. R.status().watchers)

-- Both fixtures live in TMP, so the watcher is held twice.
vim.api.nvim_buf_delete(second, { force = true })
check(
  R.status().watchers == 1,
  "wiping one buffer released the shared directory watcher (got " .. R.status().watchers .. " watchers)"
)

-- Reloading the survivor must still work through that watcher.
vim.api.nvim_set_current_buf(first)
vim.fn.writefile({ "a", "b", "c", "d", "e", "appended" }, tmp("hn.txt"))
local ok = vim.wait(2000, function()
  return vim.api.nvim_buf_line_count(0) == 6
end, 50)
check(ok, "watcher no longer reloads the surviving buffer")

-- Releasing the last reference does close the handle.
vim.api.nvim_buf_delete(first, { force = true })
check(R.status().watchers == 0, "last release did not close the watcher, got " .. R.status().watchers)
pass("reload watcher refcount survives a wipe")
