-- Strip trailing spaces and tabs on save, except in Markdown (two trailing
-- spaces are a line break there). The trim is one undo step.

skink.on("save", function(path)
  if path == nil or path:match("%.md$") then
    return
  end
  local text = skink.text()
  local trimmed = text:gsub("[ \t]+\n", "\n")
  if trimmed ~= text then
    local line, col = skink.cursor()
    skink.set_text(trimmed)
    skink.set_cursor(line, col)
    skink.action("Save") -- saving from a handler does not fire "save" again
  end
end)
