-- ashd (Ash) colourscheme: flat, amber accent, no italics.
vim.cmd("highlight clear")
if vim.fn.exists("syntax_on") == 1 then
  vim.cmd("syntax reset")
end
vim.o.termguicolors = true
vim.o.background = "dark"
vim.g.colors_name = "ashd"

local p = require("ashd.palette")
local hl = function(group, spec)
  vim.api.nvim_set_hl(0, group, spec)
end

-- Editor
hl("Normal", { fg = p.fg, bg = p.bg })
hl("NormalNC", { fg = p.fg, bg = p.bg })
hl("NormalFloat", { fg = p.fg, bg = p.surface })
hl("FloatBorder", { fg = p.line, bg = p.surface })
hl("FloatTitle", { fg = p.accent, bg = p.surface, bold = true })
hl("EndOfBuffer", { fg = p.bg })
hl("SignColumn", { bg = p.bg })
hl("LineNr", { fg = p.comment })
hl("CursorLineNr", { fg = p.accent, bold = true })
hl("CursorLine", { bg = p.surface })
hl("CursorColumn", { bg = p.surface })
hl("ColorColumn", { bg = p.surface })
hl("Cursor", { fg = p.bg, bg = p.accent })
hl("Visual", { bg = p.line })
hl("VisualNOS", { bg = p.line })
hl("Search", { fg = p.bg, bg = p.warn })
hl("IncSearch", { fg = p.bg, bg = p.accent })
hl("CurSearch", { fg = p.bg, bg = p.accent })
hl("MatchParen", { fg = p.accent, bold = true, underline = true })
hl("NonText", { fg = p.line })
hl("Whitespace", { fg = p.line })
hl("SpecialKey", { fg = p.line })
hl("Folded", { fg = p.muted, bg = p.surface })
hl("FoldColumn", { fg = p.comment, bg = p.bg })
hl("WinSeparator", { fg = p.line })
hl("VertSplit", { fg = p.line })
hl("Directory", { fg = p.accent })
hl("Title", { fg = p.accent, bold = true })
hl("ErrorMsg", { fg = p.err })
hl("WarningMsg", { fg = p.warn })
hl("MoreMsg", { fg = p.ok })
hl("Question", { fg = p.info })
hl("ModeMsg", { fg = p.fg, bold = true })

-- Menus and bars
hl("Pmenu", { fg = p.fg, bg = p.surface })
hl("PmenuSel", { fg = p.bg, bg = p.accent })
hl("PmenuSbar", { bg = p.line })
hl("PmenuThumb", { bg = p.muted })
hl("StatusLine", { fg = p.fg, bg = p.surface })
hl("StatusLineNC", { fg = p.muted, bg = p.surface })
hl("TabLine", { fg = p.muted, bg = p.surface })
hl("TabLineSel", { fg = p.bg, bg = p.accent, bold = true })
hl("TabLineFill", { bg = p.surface })
hl("WinBar", { fg = p.muted, bg = p.bg })
hl("WinBarNC", { fg = p.comment, bg = p.bg })

-- Syntax
hl("Comment", { fg = p.comment })
hl("Constant", { fg = p.fg })
hl("String", { fg = p.string })
hl("Character", { fg = p.string })
hl("Number", { fg = p.number })
hl("Boolean", { fg = p.keyword })
hl("Float", { fg = p.number })
hl("Identifier", { fg = p.fg })
hl("Function", { fg = p.fg, bold = true })
hl("Statement", { fg = p.keyword })
hl("Conditional", { fg = p.keyword })
hl("Repeat", { fg = p.keyword })
hl("Label", { fg = p.keyword })
hl("Operator", { fg = p.muted })
hl("Keyword", { fg = p.keyword })
hl("Exception", { fg = p.keyword })
hl("PreProc", { fg = p.muted })
hl("Include", { fg = p.keyword })
hl("Define", { fg = p.keyword })
hl("Macro", { fg = p.keyword })
hl("Type", { fg = p.info })
hl("StorageClass", { fg = p.keyword })
hl("Structure", { fg = p.info })
hl("Typedef", { fg = p.info })
hl("Special", { fg = p.muted })
hl("SpecialChar", { fg = p.muted })
hl("Delimiter", { fg = p.muted })
hl("Underlined", { underline = true })
hl("Error", { fg = p.err })
hl("Todo", { fg = p.bg, bg = p.warn, bold = true })

-- Treesitter (link to the classic groups, plus a few specifics)
local links = {
  ["@variable"] = "Identifier",
  ["@variable.builtin"] = "Keyword",
  ["@constant"] = "Constant",
  ["@constant.builtin"] = "Keyword",
  ["@module"] = "Identifier",
  ["@string"] = "String",
  ["@string.escape"] = "Special",
  ["@number"] = "Number",
  ["@boolean"] = "Boolean",
  ["@function"] = "Function",
  ["@function.call"] = "Function",
  ["@function.builtin"] = "Function",
  ["@keyword"] = "Keyword",
  ["@keyword.function"] = "Keyword",
  ["@keyword.return"] = "Keyword",
  ["@operator"] = "Operator",
  ["@type"] = "Type",
  ["@type.builtin"] = "Type",
  ["@property"] = "Identifier",
  ["@punctuation"] = "Delimiter",
  ["@comment"] = "Comment",
  ["@tag"] = "Keyword",
  ["@tag.attribute"] = "Identifier",
  ["@tag.delimiter"] = "Delimiter",
}
for group, target in pairs(links) do
  hl(group, { link = target })
end

-- Diagnostics
hl("DiagnosticError", { fg = p.err })
hl("DiagnosticWarn", { fg = p.warn })
hl("DiagnosticInfo", { fg = p.info })
hl("DiagnosticHint", { fg = p.muted })
hl("DiagnosticOk", { fg = p.ok })
hl("DiagnosticUnderlineError", { undercurl = true, sp = p.err })
hl("DiagnosticUnderlineWarn", { undercurl = true, sp = p.warn })
hl("DiagnosticUnderlineInfo", { undercurl = true, sp = p.info })
hl("DiagnosticUnderlineHint", { undercurl = true, sp = p.muted })

-- Diff and git
hl("DiffAdd", { bg = "#232A20" })
hl("DiffChange", { bg = "#2A2A22" })
hl("DiffDelete", { fg = p.err, bg = "#2E2220" })
hl("DiffText", { bg = "#3A3A2A" })
hl("Added", { fg = p.ok })
hl("Changed", { fg = p.warn })
hl("Removed", { fg = p.err })
hl("GitSignsAdd", { fg = p.ok })
hl("GitSignsChange", { fg = p.warn })
hl("GitSignsDelete", { fg = p.err })

-- Plugins that do not derive everything from the groups above
hl("TelescopeBorder", { fg = p.line, bg = p.surface })
hl("TelescopeSelection", { fg = p.fg, bg = p.line })
hl("TelescopeMatching", { fg = p.accent, bold = true })
hl("NeoTreeNormal", { fg = p.fg, bg = p.surface })
hl("NeoTreeNormalNC", { fg = p.fg, bg = p.surface })
hl("NeoTreeDirectoryName", { fg = p.fg })
hl("NeoTreeDirectoryIcon", { fg = p.accent })
hl("IndentBlanklineChar", { fg = p.line })
hl("IblIndent", { fg = p.line })
hl("IblScope", { fg = p.muted })

-- Terminal colours (match kitty's Ash palette)
local term = {
  "#161615", p.err, p.ok, p.warn, p.info, "#B79AC4", "#86BFB5", "#C2BEB4",
  "#6B6A64", "#E8947E", "#B3D09E", "#EED27A", "#A6C4E4", "#C9B0D3", "#A0D0C7", p.fg,
}
for i, c in ipairs(term) do
  vim.g["terminal_color_" .. (i - 1)] = c
end

-- Per-theme palette written by `ashd theme apply`: generated/nvim.lua is a module whose
-- apply() sets the highlight groups (ARCHITECTURE.md section 8). $ASHD_STATE_DIR
-- overrides the whole state directory; the file is optional (Ash above is the default).
local state = vim.env.ASHD_STATE_DIR
  or ((vim.env.XDG_STATE_HOME or vim.fn.expand("~/.local/state")) .. "/ashd")
local ok, generated = pcall(dofile, state .. "/generated/nvim.lua")
if ok and type(generated) == "table" and type(generated.apply) == "function" then
  pcall(generated.apply)
end
