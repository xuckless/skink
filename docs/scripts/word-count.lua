-- Word count in the status bar, refreshed when a file opens, after typing
-- pauses and on save. Copy into ~/.config/skink/init.lua.

local function count()
  local n = 0
  for _ in skink.text():gmatch("%S+") do
    n = n + 1
  end
  skink.set_status("words", n .. " words")
end

skink.on("open", count)
skink.on("change", count)
skink.on("save", count)
