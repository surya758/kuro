-- Skips opening and ending sequences using intervals supplied by kuro.
--
-- Written to a temp file and passed as `--script` only when skip times were
-- actually found, so mpv never loads it otherwise.
--
-- Options arrive as `kuroskip-op_start=…` etc. via `--script-opt`. A value of -1
-- means "no interval of this kind".

local options = require 'mp.options'
local msg = require 'mp.msg'

local o = {
    op_start = -1,
    op_end = -1,
    ed_start = -1,
    ed_end = -1,
}
options.read_options(o, "kuroskip")

-- Each range fires at most once, so seeking back into an opening to rewatch it
-- doesn't fight the user.
local done = { op = false, ed = false }

local function try_skip(kind, from, to, pos)
    if done[kind] or to <= 0 or to <= from then
        return false
    end
    if pos >= from and pos < to then
        done[kind] = true
        mp.set_property_number("time-pos", to)
        mp.osd_message("kuro: skipped " .. (kind == "op" and "opening" or "ending"), 2)
        msg.info(string.format("skipped %s (%.1f -> %.1f)", kind, pos, to))
        return true
    end
    return false
end

mp.observe_property("time-pos", "number", function(_, pos)
    if pos == nil then
        return
    end
    if try_skip("op", o.op_start, o.op_end, pos) then
        return
    end
    try_skip("ed", o.ed_start, o.ed_end, pos)
end)
