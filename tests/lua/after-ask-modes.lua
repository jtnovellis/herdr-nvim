-- Where a delivered message leaves you: `after_ask` / `after_send`, and the
-- three booleans they replaced.
--
-- The legacy keys still have to work -- a config written against 0.2.3 must not
-- start doing the opposite of what it says -- and every translation has to be
-- visible in `:checkhealth`, because a silently rewritten option is how you end
-- up reading documentation for a plugin you are not running.
local H = require("herdr-nvim")

local function setup(opts)
  H.config = vim.deepcopy(require("herdr-nvim").config)
  -- setup() merges into the live config, so start every case from the defaults.
  for key, value in pairs({ after_ask = "focus", after_send = "focus" }) do
    H.config[key] = value
  end
  H.setup(vim.tbl_extend("force", { keymaps = false, notify = false }, opts or {}))
end

-- The defaults: one ask lands you in the agent's pane.
setup({})
check(H.mode("after_ask") == "focus", "after_ask default is not focus: " .. tostring(H.mode("after_ask")))
check(H.mode("after_send") == "focus", "after_send default is not focus: " .. tostring(H.mode("after_send")))
check(#H._deprecated == 0, "a clean setup recorded a deprecation")

-- `focus_after_ask = true` promised "instead of reading the reply here" and
-- delivered both. It means only the promise now.
setup({ focus_after_ask = true })
check(H.mode("after_ask") == "focus", "focus_after_ask=true did not become focus")
check(#H._deprecated == 1, "focus_after_ask was translated without saying so")
check(H._deprecated[1].replacement == "after_ask", "the record names the wrong replacement")

-- Turning the float off used to be the only way to stop it. It must not now
-- mean "focus", which would move you somewhere you did not ask to go.
setup({ reply = { enabled = false } })
check(H.mode("after_ask") == "stay", "reply.enabled=false did not become stay")
check(H.config.reply == nil, "a translated legacy key survived into the config")

-- Asking for the float explicitly still gets the float.
setup({ reply = { enabled = true } })
check(H.mode("after_ask") == "reply", "reply.enabled=true did not become reply")

-- Both, contradicting each other: focus wins, because that is what the old
-- option's own documentation promised.
setup({ focus_after_ask = true, reply = { enabled = false } })
check(H.mode("after_ask") == "focus", "focus lost to reply.enabled=false")
check(#H._deprecated == 2, "only one of the two legacy keys was reported")

-- The old default, written out: no focus, so the float.
setup({ focus_after_ask = false })
check(H.mode("after_ask") == "reply", "focus_after_ask=false did not keep the old float")

setup({ focus_after_send = true })
check(H.mode("after_send") == "focus", "focus_after_send=true did not become focus")
setup({ focus_after_send = false })
check(H.mode("after_send") == "stay", "focus_after_send=false did not become stay")

-- A config carrying both spellings meant the one that still exists.
setup({ after_ask = "stay", focus_after_ask = true })
check(H.mode("after_ask") == "stay", "a legacy key overrode an explicit after_ask")

-- A typo must fall back to something the callers understand rather than
-- silently disabling the route, and must say so: a WARN is the whole point, so
-- it is captured here rather than left to scribble over the suite's output.
local warned = {}
local real_notify = vim.notify
vim.notify = function(msg)
  table.insert(warned, msg)
end
setup({ after_ask = "focsu" })
vim.notify = real_notify
check(H.mode("after_ask") == "focus", "an unusable mode did not fall back: " .. tostring(H.mode("after_ask")))
check(#warned == 1 and warned[1]:find("focsu", 1, true), "an unusable mode was rejected silently")

-- setup() is called with the user's own table; it must not rewrite their keys.
local mine = { keymaps = false, notify = false, focus_after_ask = true }
H.setup(mine)
check(mine.focus_after_ask == true, "setup() mutated the caller's options table")

pass("after_ask / after_send modes")
