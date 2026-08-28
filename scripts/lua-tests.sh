#!/usr/bin/env bash
# Headless Neovim checks for the Lua side. Run: scripts/lua-tests.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PRELUDE="$TMP/prelude.lua"
cat > "$PRELUDE" <<'LUA'
_G.check = function(cond, msg)
  if not cond then
    io.stderr:write("FAIL: " .. msg .. "\n")
    vim.cmd("cquit 1")
  end
end
_G.pass = function(name) io.stdout:write("ok  " .. name .. "\n") end
vim.g.mapleader = " "
LUA

run() {
  local name="$1"; shift
  nvim --clean --headless --cmd "set rtp+=$ROOT" --cmd "luafile $PRELUDE" -c "runtime plugin/herdr-nvim.lua" "$@" -c "qa!" \
    || { echo "FAILED: $name"; exit 1; }
}

printf 'a\nb\nc\nd\ne\n' > "$TMP/hn.txt"
printf 'x1\nx2\nx3\n' > "$TMP/other.txt"

run "visual annotate" -c "lua vim.ui.input = function(_, cb) cb('note') end" -c "e $TMP/hn.txt" -c "lua
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('Vj ac', true, false, true), 'x', false)
  local l = require('herdr-nvim.annotations').list()
  check(vim.fn.mode() == 'n', 'still in visual mode: ' .. vim.fn.mode())
  check(#l == 1, 'expected 1 annotation, got ' .. #l)
  check(l[1].row == 0 and l[1].end_row == 1, 'rows ' .. tostring(l[1].row) .. '-' .. tostring(l[1].end_row))
  check(require('herdr-nvim').statusline() == '● 1', 'statusline: ' .. require('herdr-nvim').statusline())
  pass('visual annotate')"

run "reload survival" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  A.add(0, 1, 2, 't')
  vim.fn.writefile({'a','b','c','d','e','f'}, '$TMP/hn.txt')
  vim.cmd('checktime')
  check(vim.api.nvim_buf_line_count(0) == 6, 'autoread did not reload')
  local l = A.list()
  check(#l == 1 and not l[1].stale and l[1].row == 1, 'lost or stale after unchanged-range reload')
  -- Neovim detects changes by mtime/size: change the size too.
  vim.fn.writefile({'a','BBBB','c','d','e','f'}, '$TMP/hn.txt')
  vim.cmd('checktime')
  check(vim.api.nvim_buf_get_lines(0, 1, 2, false)[1] == 'BBBB', 'second reload did not happen')
  local l2 = A.list({ include_stale = true })
  check(#l2 == 1 and l2[1].stale, 'expected stale after the annotated line changed')
  check(A.count() == 0, 'stale items must not count as pending')
  vim.cmd('e!')
  check(#A.list({ include_stale = true }) == 1, 'lost on :e!')
  pass('reload survival')"

run "undo revive" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  A.add(0, 1, 1, 't')
  vim.cmd('normal! 2Gdd')
  check(#A.list() == 0 and #A.list({include_stale=true}) == 1, 'expected stale after dd')
  vim.cmd('normal! u')
  local l = A.list()
  check(#l == 1 and l[1].row == 1, 'undo did not revive the annotation')
  pass('undo revive')"

run "overlap edits" -c "lua vim.ui.input = function(o, cb) cb(o.default ~= '' and 'edited' or 'first') end" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  local H = require('herdr-nvim')
  vim.api.nvim_win_set_cursor(0, {1, 0}); H.annotate({ line1 = 1, line2 = 3 })
  H.annotate({ line1 = 2, line2 = 4 })
  local l = A.list()
  check(#l == 1, 'overlapping range duplicated: ' .. #l)
  check(l[1].text == 'edited', 'overlap should edit: ' .. l[1].text)
  pass('overlap edits')"

run "gravity" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  A.add(0, 1, 1, 't')
  vim.api.nvim_buf_set_lines(0, 1, 1, false, {'inserted above'})
  local l = A.list()
  check(l[1].row == 2 and l[1].end_row == 2, 'annotation swallowed inserted line: ' .. l[1].row .. '-' .. l[1].end_row)
  pass('gravity')"

run "setup semantics" -c "lua
  vim.keymap.set('n', '<leader>al', function() end, { desc = 'mine' })
  local H = require('herdr-nvim')
  H.setup({ prompt = 'X: ' })
  H.setup({ notify = false })
  check(H.config.prompt == 'X: ' and H.config.notify == false, 'second setup reset options')
  check(vim.fn.maparg('<leader>al', 'n', false, true).desc == 'mine', 'user mapping clobbered')
  check(vim.fn.maparg('<leader>ac', 'n', false, true).desc ~= nil, 'our mapping missing')
  H.setup({ keymaps = false })
  check(vim.fn.maparg('<leader>al', 'n', false, true).desc == 'mine', 'keymaps=false removed the user mapping')
  check(vim.fn.maparg('<leader>ac', 'n') == '', 'keymaps=false left our mapping')
  check(H.quit_guard_should_intercept({ uis = 1, windows = 1 }), 'guard should intercept')
  check(not H.quit_guard_should_intercept({ uis = 0, windows = 1 }), 'no UI: no intercept')
  check(not H.quit_guard_should_intercept({ uis = 1, windows = 2 }), 'two windows: no intercept')
  pass('setup semantics + quit guard helper')"

run "winfixbuf jump + restore" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  local UI = require('herdr-nvim.ui')
  local file_win = vim.api.nvim_get_current_win()
  local file_buf = vim.api.nvim_get_current_buf()
  vim.cmd('edit $TMP/other.txt'); local other_buf = vim.api.nvim_get_current_buf()
  A.add(0, 0, 0, 'in other')
  vim.api.nvim_win_set_buf(file_win, file_buf)
  vim.api.nvim_win_set_cursor(file_win, {4, 0})
  vim.cmd('vsplit'); local tree_win = vim.api.nvim_get_current_win()
  local scratch = vim.api.nvim_create_buf(false, true); vim.api.nvim_win_set_buf(tree_win, scratch)
  vim.wo[tree_win].winfixbuf = true
  UI.open()
  check(UI.is_open(), 'float did not open')
  check(vim.api.nvim_win_get_buf(tree_win) == scratch, 'winfixbuf window was changed')
  check(vim.api.nvim_win_get_buf(file_win) == other_buf, 'preview did not use the normal window')
  UI.close()
  check(vim.api.nvim_win_get_buf(file_win) == file_buf, 'origin window not restored (from a non-normal origin)')
  vim.api.nvim_set_current_win(file_win)
  UI.open()
  check(vim.api.nvim_win_get_buf(file_win) == other_buf, 'hover did not preview')
  UI.close()
  check(vim.api.nvim_win_get_buf(file_win) == file_buf and vim.api.nvim_win_get_cursor(file_win)[1] == 4, 'origin buffer/cursor not restored')
  vim.api.nvim_set_current_win(file_win)
  UI.open(); UI.goto_item()
  check(vim.api.nvim_win_get_buf(file_win) == other_buf, 'goto did not commit the jump')
  pass('winfixbuf jump + restore')"

run "reload watcher" -c "e $TMP/hn.txt" -c "lua
  local A = require('herdr-nvim.annotations')
  A.add(0, 0, 0, 'first line')
  require('herdr-nvim.reload').start({ debounce_ms = 50, force_without_ui = true })
  vim.fn.writefile({'a','b','c','d','e','appended'}, '$TMP/hn.txt')
  local ok = vim.wait(2000, function() return vim.api.nvim_buf_line_count(0) == 6 end, 50)
  check(ok, 'watcher did not reload the buffer')
  check(#A.list() == 1 and not A.list()[1].stale, 'annotation lost on watcher reload')
  vim.api.nvim_buf_set_lines(0, 5, 6, false, {'local edit'})
  vim.fn.writefile({'a','b','c','d','e','disk'}, '$TMP/hn.txt')
  vim.wait(500, function() return false end, 50)
  check(vim.api.nvim_buf_get_lines(0, 5, 6, false)[1] == 'local edit', 'modified buffer was clobbered')
  check(vim.b.herdr_nvim_stale == true, 'modified buffer not flagged stale')
  pass('reload watcher')"

run "health + preview + encode" -c "lua
  local bridge = require('herdr-nvim.bridge')
  local json = bridge.encode({ code = 'caf\xe9 bad', ok = true })
  check(type(json) == 'string' and json:find('ok'), 'encode failed on invalid utf-8')
  vim.cmd('checkhealth herdr-nvim')
  local lines = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
  check(lines:find('binary:'), 'health missing binary line')
  check(not lines:find('ERROR'), 'health reported an ERROR:\n' .. lines)
  pass('health + encode')"

run "user autocmd + statusline" -c "e $TMP/hn.txt" -c "lua
  local fired = 0
  vim.api.nvim_create_autocmd('User', { pattern = 'HerdrNvimAnnotationsChanged', callback = function() fired = fired + 1 end })
  local A = require('herdr-nvim.annotations')
  A.add(0, 0, 0, 'x'); A.add(0, 2, 2, 'y')
  vim.wait(200, function() return fired > 0 end, 10)
  check(fired >= 1, 'User autocmd not fired')
  check(require('herdr-nvim').statusline() == '● 2', 'statusline count')
  A.mark_delivered()
  check(require('herdr-nvim').statusline() == '', 'delivered items still pending')
  check(A.total() == 2, 'delivered items were dropped')
  check(A.reap_stale() == 2 and A.total() == 0, 'reap did not remove delivered items')
  pass('user autocmd + statusline')"

run "picker" -c "e $TMP/hn.txt" -c "lua
  local P = require('herdr-nvim.picker')
  local cands = {
    { path = '$TMP/other.txt', line = 3, session = true, touched_unix = os.time() - 120, diff_stat = { 2, 1 } },
    { path = '$TMP/hn.txt', session = true, newly_created = true },
    { path = '/nowhere/repo/deep/target.rs', session = false },
  }
  for _, c in ipairs(cands) do c.display = c.path end
  local m = P.rank(cands, '', 1)
  check(#m == 1 and m[1].cand.path == '$TMP/other.txt', 'empty query caps to max_files and keeps session rows')
  local m2 = P.rank(cands, 'target', 20)
  check(#m2 >= 1 and m2[1].cand.path == '/nowhere/repo/deep/target.rs', 'typed query reaches repo-only rows: ' .. vim.inspect(m2))
  P.open({ candidates = vim.deepcopy(cands), cwd = '$TMP', max_files = 20 }, { force_without_ui = true })
  check(P.is_open() == true, 'picker did not open')
  local floats = 0
  for _, w in ipairs(vim.api.nvim_list_wins()) do if vim.api.nvim_win_get_config(w).relative == 'editor' then floats = floats + 1 end end
  check(floats == 2, 'expected prompt + list floats, got ' .. floats)
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<CR>', true, false, true), 'x', false)
  check(not P.is_open(), 'picker still open after <CR>')
  check(vim.fn.expand('%:t') == 'other.txt' and vim.fn.line('.') == 3, 'did not open other.txt:3, got ' .. vim.fn.expand('%:t') .. ':' .. vim.fn.line('.'))
  -- handoff file with JSON nulls, deferred until a UI attaches
  local f = '$TMP/handoff.json'
  vim.fn.writefile({ vim.json.encode({ candidates = { { path = '$TMP/hn.txt', line = vim.NIL, session = true, diff_stat = vim.NIL } }, cwd = '$TMP', max_files = 20 }) }, f)
  check(P.open_file(f) == true, 'open_file failed')
  check(vim.fn.filereadable(f) == 0, 'handoff not deleted')
  check(not P.is_open(), 'picker opened without a UI')
  vim.api.nvim_exec_autocmds('UIEnter', {})
  vim.wait(200, function() return P.is_open() end, 10)
  check(P.is_open(), 'picker did not open on UIEnter')
  P.close()
  pass('picker')"

echo "all Lua checks passed"
