-- Opt in with HERDR_NVIM_TEST_DIFFVIEW=/path/to/diffview.nvim.
local diffview_path = vim.env.HERDR_NVIM_TEST_DIFFVIEW
if not diffview_path or diffview_path == "" then
  io.stdout:write("skip  Diffview integration (HERDR_NVIM_TEST_DIFFVIEW not set)\n")
  return
end

local function run()
  check(vim.fn.isdirectory(diffview_path .. "/lua/diffview") == 1, "invalid Diffview checkout")
  vim.opt.rtp:append(diffview_path)
  vim.cmd("runtime plugin/diffview.lua")
  require("diffview").setup({ use_icons = false, view = { default = { layout = "diff2_horizontal" } } })
  local H = require("herdr-nvim")
  local A = require("herdr-nvim.annotations")
  local bridge = require("herdr-nvim.bridge")
  local binary = bridge.binary(H.config)
  check(binary ~= nil, "build herdr-nvim before the integration test")
  H.config.notify = false
  vim.notify = function() end
  vim.ui.input = function(_, cb)
    cb("review this revision")
  end

  local repo = tmp("diffview-repo")
  vim.fn.mkdir(repo, "p")
  repo = vim.uv.fs_realpath(repo)
  local function git(...)
    local args = { "git", "-C", repo }
    vim.list_extend(args, { ... })
    local result = vim.system(args, { text = true }):wait()
    check(result.code == 0, "git failed: " .. (result.stderr or ""))
    return vim.trim(result.stdout)
  end
  local function write(path, lines)
    vim.fn.writefile(lines, repo .. "/" .. path)
  end
  git("init", "-q")
  git("config", "user.name", "Test")
  git("config", "user.email", "test@example.com")
  git("config", "commit.gpgsign", "false")
  git("config", "core.hooksPath", "/dev/null")
  write("historical name.lua", { "old name", "same contents", "third line" })
  write("changed.lua", { "before", "second line", "third line" })
  write("deleted.lua", { "deleted first", "deleted second" })
  git("add", ".")
  git("commit", "-qm", "base")
  local left = git("rev-parse", "HEAD")
  git("mv", "historical name.lua", "renamed name.lua")
  git("rm", "-q", "deleted.lua")
  write("changed.lua", { "after", "second line", "third line" })
  write("added.lua", { "added first", "added second" })
  git("add", ".")
  git("commit", "-qm", "changes")
  local right = git("rev-parse", "HEAD")
  vim.fn.chdir(repo)

  local diffview = require("diffview")
  local lib = require("diffview.lib")
  local function open(args)
    diffview.open(args)
    local view = lib.get_current_view()
    check(view ~= nil, "Diffview did not open")
    check(
      vim.wait(10000, function()
        local layout = view.cur_layout
        return view.cur_entry
          and view.cur_entry.opened
          and not view.nulled
          and layout
          and layout.a
          and layout.b
          and layout.a:is_file_open()
          and layout.b:is_file_open()
      end, 10),
      "Diffview buffers did not load"
    )
    return view
  end
  local function annotate(view, symbol, path, revision, side, visual)
    local win = view.cur_layout[symbol]
    vim.api.nvim_set_current_win(win.id)
    local buf = vim.api.nvim_get_current_buf()
    local name, buftype = vim.api.nvim_buf_get_name(buf), vim.bo[buf].buftype
    if visual then
      vim.wo[win.id].foldenable = false
      vim.api.nvim_win_set_cursor(0, { 1, 0 })
      vim.api.nvim_feedkeys("Vj", "x", false)
      H.annotate()
    else
      H.annotate({ line1 = 1, line2 = 2 })
    end
    check(vim.api.nvim_buf_get_name(buf) == name and vim.bo[buf].buftype == buftype, "adapter mutated buffer")
    local item = A.find_overlapping(buf, 0, 1, require("herdr-nvim.diffview").current(buf))
    check(item ~= nil, "annotation missing for " .. side .. " " .. path)
    check(item.file == repo .. "/" .. path, "wrong original path: " .. item.file)
    check(item.diff.repo_root == repo and item.diff.path == path, "wrong repository metadata")
    check(item.diff.revision == revision and item.diff.side == side, "wrong revision or side")
    check(item.row == 0 and item.end_row == 1, "selection range changed")
    return item
  end

  local captured
  bridge.run = function(_, args, stdin, cb)
    check(args[1] == "send", "unexpected bridge call")
    captured = stdin
    cb(false, "captured without sending")
  end
  local function transport()
    -- Deliver from an ordinary tab: source context must come from the queued note.
    local tab = vim.api.nvim_get_current_tabpage()
    vim.cmd("tabnew")
    H.paste()
    check(captured ~= nil, "transport payload missing")
    local payload = vim.json.decode(captured)
    local result = vim.system({ binary, "send", "--dry-run" }, { text = true, stdin = captured }):wait()
    check(result.code == 0, "dry run failed: " .. (result.stdout or "") .. (result.stderr or ""))
    local rendered = vim.json.decode(result.stdout)
    check(rendered.ok == true and rendered.dry_run == true, "not a dry run")
    for _, comment in ipairs(payload.comments) do
      local diff = comment.diff
      check(comment.line == 1 and comment.end_line == 2 and comment.code:find("\n"), "transport lost selection")
      if diff then
        check(comment.file == repo .. "/" .. diff.path, "transport lost original file")
        local context = "Diffview: "
          .. diff.side
          .. " side, revision "
          .. diff.revision
          .. "\nRepository: "
          .. repo
          .. "\nRevision path: "
          .. diff.path
          .. "\n"
        check(rendered.prompt:find(context, 1, true) ~= nil, "rendered prompt lost revision context")
        check(rendered.prompt:find(diff.path .. ":1-2", 1, true) ~= nil, "rendered prompt lost line range")
      else
        check(comment.file == repo .. "/changed.lua", "normal annotation path changed")
      end
    end
    check(not rendered.prompt:find("diffview://", 1, true), "pseudo URI leaked to agent")
    vim.cmd("tabclose!")
    vim.api.nvim_set_current_tabpage(tab)
    A.clear()
  end

  local view = open({ left .. ".." .. right, "--", "renamed name.lua", "historical name.lua" })
  annotate(view, "a", "historical name.lua", left, "left", true)
  annotate(view, "b", "renamed name.lua", right, "right")
  check(#A.list() == 2, "rename sides were merged")
  transport()
  diffview.close()

  view = open({ left .. ".." .. right, "--", "changed.lua" })
  annotate(view, "a", "changed.lua", left, "left")
  annotate(view, "b", "changed.lua", right, "right")
  local first_tab = view.tabpage
  local reverse = open({ right .. ".." .. left, "--", "changed.lua" })
  annotate(reverse, "a", "changed.lua", right, "left")
  annotate(reverse, "b", "changed.lua", left, "right")
  check(#A.list() == 4, "shared revision buffers lost distinct comparison sides")
  annotate(reverse, "a", "changed.lua", right, "left")
  check(#A.list() == 4, "same source overlap duplicated a note")
  transport()
  diffview.close()
  vim.api.nvim_set_current_tabpage(first_tab)
  diffview.close()

  view = open({ left .. ".." .. right, "--", "added.lua" })
  vim.api.nvim_set_current_win(view.cur_layout.a.id)
  H.annotate()
  check(A.total() == 0, "accepted the absent side of an added file")
  annotate(view, "b", "added.lua", right, "right")
  transport()
  diffview.close()

  view = open({ left .. ".." .. right, "--", "deleted.lua" })
  annotate(view, "a", "deleted.lua", left, "left")
  vim.api.nvim_set_current_win(view.cur_layout.b.id)
  H.annotate()
  check(A.total() == 1, "accepted the absent side of a deleted file")
  transport()
  diffview.close()

  write("changed.lua", { "working tree", "second line", "third line" })
  vim.cmd("edit " .. vim.fn.fnameescape(repo .. "/changed.lua"))
  H.annotate({ line1 = 1, line2 = 2 })
  local ordinary = A.list()[1]
  check(ordinary.diff == nil, "ordinary file acquired Diffview context")
  view = open({ "HEAD", "--", "changed.lua" })
  annotate(view, "a", "changed.lua", right, "left")
  annotate(view, "b", "changed.lua", "WORKTREE", "right")
  check(#A.list() == 3 and A.get(ordinary.id).diff == nil, "working-tree overlap relabeled ordinary note")
  transport()
  diffview.close()

  git("add", "changed.lua")
  view = open({ "--cached", "--", "changed.lua" })
  annotate(view, "a", "changed.lua", right, "left")
  annotate(view, "b", "changed.lua", "INDEX:0", "right")
  transport()
  diffview.close()
  pass("Diffview revisions, rename/add/delete sides, working tree, index, and dry-run transport")
end

local ok, err = xpcall(run, debug.traceback)
check(ok, err)
