local p = require("ashd.palette")

local function mode(color)
  return {
    a = { fg = p.bg, bg = color, gui = "bold" },
    b = { fg = p.fg, bg = p.line },
    c = { fg = p.muted, bg = p.surface },
  }
end

return {
  normal = mode(p.accent),
  insert = mode(p.ok),
  visual = mode(p.info),
  replace = mode(p.err),
  command = mode(p.warn),
  inactive = {
    a = { fg = p.muted, bg = p.surface },
    b = { fg = p.muted, bg = p.surface },
    c = { fg = p.muted, bg = p.surface },
  },
}
