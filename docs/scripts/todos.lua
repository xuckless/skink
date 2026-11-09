-- :todos lists the TODO / FIXME lines of the current file in a picker and
-- jumps to the one you choose. Ctrl-Alt-J opens it too (an unbound key, so
-- it reaches the script's key handler).

local function todos()
  local lines, items = {}, {}
  local n = 0
  for text in (skink.text() .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    if text:match("TODO") or text:match("FIXME") then
      lines[#lines + 1] = n
      items[#items + 1] = n .. ": " .. text:gsub("^%s+", "")
    end
  end
  if #items == 0 then
    skink.message("No TODO or FIXME in this file")
    return
  end
  skink.pick("TODOs", items, function(_, i)
    skink.set_cursor(lines[i])
  end)
end

skink.add_command("todos", todos)

-- A script command cannot be run from inside a script (skink.command
-- "todos" here would be refused), so call the function itself.
skink.on("key", function(key)
  if key == "Ctrl-Alt-J" then
    todos()
    return true
  end
end)
