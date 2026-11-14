-- Highlight definitions. Everything is read from M.colors (the 17 design tokens plus a few
-- derived tints), so a theme switch only swaps the table above.
--
-- Usage:  dofile(vim.fn.expand("~/.local/state/ashd/generated/nvim.lua")).apply()
--
-- Design notes (docs/design/TOKENS.md): one accent, four background steps, two text steps;
-- syntax uses only keyword / string / number / comment, everything else is fg.

function M.highlights()
  local c = M.colors
  local none = "NONE"
  local h = {}

  -- ── editor UI ────────────────────────────────────────────────────────────
  h.Normal = { fg = c.fg, bg = c.surface_2 }
  h.NormalNC = { fg = c.fg, bg = c.surface_2 }
  h.NormalFloat = { fg = c.fg, bg = c.surface }
  h.FloatBorder = { fg = c.frame, bg = c.surface }
  h.FloatTitle = { fg = c.fg, bg = c.surface, bold = true }
  h.FloatFooter = { fg = c.fg_muted, bg = c.surface }
  h.Cursor = { fg = c.on_accent, bg = c.accent }
  h.lCursor = { link = "Cursor" }
  h.CursorIM = { link = "Cursor" }
  h.TermCursor = { link = "Cursor" }
  h.TermCursorNC = { fg = c.on_accent, bg = c.fg_muted }
  h.CursorLine = { bg = c.line }
  h.CursorColumn = { bg = c.line }
  h.ColorColumn = { bg = c.surface }
  h.CursorLineNr = { fg = c.fg, bg = c.line }
  h.CursorLineSign = { bg = c.line }
  h.CursorLineFold = { bg = c.line }
  h.LineNr = { fg = c.syn_comment }
  h.LineNrAbove = { link = "LineNr" }
  h.LineNrBelow = { link = "LineNr" }
  h.SignColumn = { fg = c.syn_comment, bg = none }
  h.FoldColumn = { fg = c.syn_comment, bg = none }
  h.Folded = { fg = c.fg_muted, bg = c.surface }
  h.Conceal = { fg = c.fg_muted }
  h.Directory = { fg = c.accent }
  h.EndOfBuffer = { fg = c.surface_2 }
  h.ErrorMsg = { fg = c.err }
  h.WarningMsg = { fg = c.warn }
  h.ModeMsg = { fg = c.fg }
  h.MoreMsg = { fg = c.ok }
  h.MsgArea = { fg = c.fg, bg = c.surface }
  h.MsgSeparator = { fg = c.line, bg = c.surface }
  h.Question = { fg = c.accent }
  h.NonText = { fg = c.line }
  h.SpecialKey = { fg = c.line }
  h.Whitespace = { fg = c.line }
  h.Visual = { bg = c.visual }
  h.VisualNOS = { bg = c.visual }
  h.Search = { fg = c.fg, bg = c.search }
  h.IncSearch = { fg = c.on_accent, bg = c.accent }
  h.CurSearch = { fg = c.on_accent, bg = c.accent }
  h.Substitute = { fg = c.on_accent, bg = c.err }
  h.MatchParen = { fg = c.accent, bg = c.line, bold = true }
  h.Pmenu = { fg = c.fg, bg = c.surface }
  h.PmenuSel = { fg = c.fg, bg = c.line }
  h.PmenuKind = { fg = c.fg_muted, bg = c.surface }
  h.PmenuKindSel = { fg = c.fg_muted, bg = c.line }
  h.PmenuExtra = { fg = c.fg_muted, bg = c.surface }
  h.PmenuExtraSel = { fg = c.fg_muted, bg = c.line }
  h.PmenuSbar = { bg = c.surface }
  h.PmenuThumb = { bg = c.frame }
  h.PmenuMatch = { fg = c.accent, bg = c.surface }
  h.PmenuMatchSel = { fg = c.accent, bg = c.line }
  h.WildMenu = { fg = c.on_accent, bg = c.accent }
  h.StatusLine = { fg = c.fg, bg = c.surface }
  h.StatusLineNC = { fg = c.fg_muted, bg = c.surface }
  h.StatusLineTerm = { link = "StatusLine" }
  h.StatusLineTermNC = { link = "StatusLineNC" }
  h.WinBar = { fg = c.fg, bg = none }
  h.WinBarNC = { fg = c.fg_muted, bg = none }
  h.WinSeparator = { fg = c.frame, bg = none }
  h.VertSplit = { link = "WinSeparator" }
  h.TabLine = { fg = c.fg_muted, bg = c.surface }
  h.TabLineSel = { fg = c.on_accent, bg = c.accent }
  h.TabLineFill = { bg = c.surface }
  h.Title = { fg = c.fg, bold = true }
  h.QuickFixLine = { bg = c.line }
  h.SpellBad = { sp = c.err, undercurl = true }
  h.SpellCap = { sp = c.warn, undercurl = true }
  h.SpellLocal = { sp = c.info, undercurl = true }
  h.SpellRare = { sp = c.info, undercurl = true }
  h.Underlined = { underline = true }
  h.Bold = { bold = true }
  h.Italic = { italic = true }

  -- diff
  h.DiffAdd = { bg = c.diff_add }
  h.DiffChange = { bg = c.diff_change }
  h.DiffDelete = { fg = c.err, bg = c.diff_delete }
  h.DiffText = { bg = c.diff_text }
  h.Added = { fg = c.ok }
  h.Changed = { fg = c.info }
  h.Removed = { fg = c.err }

  -- ── classic syntax groups ────────────────────────────────────────────────
  h.Comment = { fg = c.syn_comment }
  h.Constant = { fg = c.syn_number }
  h.String = { fg = c.syn_string }
  h.Character = { fg = c.syn_string }
  h.Number = { fg = c.syn_number }
  h.Boolean = { fg = c.syn_number }
  h.Float = { fg = c.syn_number }
  h.Identifier = { fg = c.fg }
  h.Function = { fg = c.fg }
  h.Statement = { fg = c.syn_keyword }
  h.Conditional = { fg = c.syn_keyword }
  h.Repeat = { fg = c.syn_keyword }
  h.Label = { fg = c.syn_keyword }
  h.Operator = { fg = c.fg_muted }
  h.Keyword = { fg = c.syn_keyword }
  h.Exception = { fg = c.syn_keyword }
  h.PreProc = { fg = c.syn_keyword }
  h.Include = { fg = c.syn_keyword }
  h.Define = { fg = c.syn_keyword }
  h.Macro = { fg = c.syn_keyword }
  h.PreCondit = { fg = c.syn_keyword }
  h.Type = { fg = c.fg }
  h.StorageClass = { fg = c.syn_keyword }
  h.Structure = { fg = c.syn_keyword }
  h.Typedef = { fg = c.syn_keyword }
  h.Special = { fg = c.fg }
  h.SpecialChar = { fg = c.info }
  h.Tag = { fg = c.syn_keyword }
  h.Delimiter = { fg = c.fg_muted }
  h.SpecialComment = { fg = c.fg_muted }
  h.Debug = { fg = c.warn }
  h.Ignore = { fg = c.syn_comment }
  h.Error = { fg = c.err }
  h.Todo = { fg = c.on_accent, bg = c.accent, bold = true }

  -- ── treesitter ───────────────────────────────────────────────────────────
  h["@variable"] = { fg = c.fg }
  h["@variable.builtin"] = { fg = c.syn_keyword }
  h["@variable.parameter"] = { fg = c.fg }
  h["@variable.parameter.builtin"] = { fg = c.fg_muted }
  h["@variable.member"] = { fg = c.fg }
  h["@constant"] = { fg = c.syn_number }
  h["@constant.builtin"] = { fg = c.syn_number }
  h["@constant.macro"] = { fg = c.syn_number }
  h["@module"] = { fg = c.fg }
  h["@module.builtin"] = { fg = c.syn_keyword }
  h["@label"] = { fg = c.syn_keyword }
  h["@string"] = { link = "String" }
  h["@string.documentation"] = { fg = c.syn_comment }
  h["@string.regexp"] = { fg = c.info }
  h["@string.escape"] = { fg = c.info }
  h["@string.special"] = { fg = c.info }
  h["@string.special.url"] = { fg = c.info, underline = true }
  h["@string.special.path"] = { fg = c.fg }
  h["@character"] = { link = "Character" }
  h["@character.special"] = { fg = c.info }
  h["@boolean"] = { link = "Boolean" }
  h["@number"] = { link = "Number" }
  h["@number.float"] = { link = "Float" }
  h["@type"] = { fg = c.fg }
  h["@type.builtin"] = { fg = c.fg }
  h["@type.definition"] = { fg = c.fg }
  h["@attribute"] = { fg = c.fg_muted }
  h["@attribute.builtin"] = { fg = c.fg_muted }
  h["@property"] = { fg = c.fg }
  h["@function"] = { fg = c.fg }
  h["@function.builtin"] = { fg = c.fg }
  h["@function.call"] = { fg = c.fg }
  h["@function.macro"] = { fg = c.syn_keyword }
  h["@function.method"] = { fg = c.fg }
  h["@function.method.call"] = { fg = c.fg }
  h["@constructor"] = { fg = c.fg }
  h["@operator"] = { link = "Operator" }
  h["@keyword"] = { fg = c.syn_keyword }
  h["@keyword.function"] = { fg = c.syn_keyword }
  h["@keyword.operator"] = { fg = c.syn_keyword }
  h["@keyword.return"] = { fg = c.syn_keyword }
  h["@keyword.import"] = { fg = c.syn_keyword }
  h["@keyword.conditional"] = { fg = c.syn_keyword }
  h["@keyword.repeat"] = { fg = c.syn_keyword }
  h["@keyword.exception"] = { fg = c.syn_keyword }
  h["@keyword.directive"] = { fg = c.syn_keyword }
  h["@punctuation"] = { fg = c.fg_muted }
  h["@punctuation.delimiter"] = { fg = c.fg_muted }
  h["@punctuation.bracket"] = { fg = c.fg_muted }
  h["@punctuation.special"] = { fg = c.syn_keyword }
  h["@comment"] = { link = "Comment" }
  h["@comment.documentation"] = { fg = c.syn_comment }
  h["@comment.error"] = { fg = c.on_accent, bg = c.err, bold = true }
  h["@comment.warning"] = { fg = c.on_accent, bg = c.warn, bold = true }
  h["@comment.todo"] = { fg = c.on_accent, bg = c.accent, bold = true }
  h["@comment.note"] = { fg = c.on_accent, bg = c.info, bold = true }
  h["@tag"] = { fg = c.syn_keyword }
  h["@tag.attribute"] = { fg = c.fg }
  h["@tag.delimiter"] = { fg = c.fg_muted }
  h["@markup"] = { fg = c.fg }
  h["@markup.strong"] = { bold = true }
  h["@markup.italic"] = { italic = true }
  h["@markup.strikethrough"] = { strikethrough = true }
  h["@markup.underline"] = { underline = true }
  h["@markup.heading"] = { fg = c.accent, bold = true }
  h["@markup.quote"] = { fg = c.fg_muted }
  h["@markup.math"] = { fg = c.info }
  h["@markup.link"] = { fg = c.info }
  h["@markup.link.label"] = { fg = c.info }
  h["@markup.link.url"] = { fg = c.info, underline = true }
  h["@markup.raw"] = { fg = c.syn_string }
  h["@markup.list"] = { fg = c.accent }
  h["@markup.list.checked"] = { fg = c.ok }
  h["@markup.list.unchecked"] = { fg = c.fg_muted }
  h["@diff.plus"] = { fg = c.ok }
  h["@diff.minus"] = { fg = c.err }
  h["@diff.delta"] = { fg = c.info }
  h["@lsp.type.comment"] = {}
  h["@lsp.type.keyword"] = { link = "@keyword" }
  h["@lsp.type.namespace"] = { link = "@module" }
  h["@lsp.type.enumMember"] = { link = "@constant" }
  h["@lsp.type.macro"] = { link = "@function.macro" }
  h["@lsp.typemod.variable.defaultLibrary"] = { link = "@variable.builtin" }
  h["@lsp.typemod.function.defaultLibrary"] = { link = "@function.builtin" }

  -- ── LSP / diagnostics ────────────────────────────────────────────────────
  h.LspReferenceText = { bg = c.line }
  h.LspReferenceRead = { bg = c.line }
  h.LspReferenceWrite = { bg = c.line, underline = true }
  h.LspReferenceTarget = { bg = c.line }
  h.LspInlayHint = { fg = c.syn_comment, bg = c.surface }
  h.LspCodeLens = { fg = c.syn_comment }
  h.LspCodeLensSeparator = { fg = c.line }
  h.LspSignatureActiveParameter = { fg = c.accent, bold = true }
  h.LspInfoBorder = { link = "FloatBorder" }
  h.DiagnosticError = { fg = c.err }
  h.DiagnosticWarn = { fg = c.warn }
  h.DiagnosticInfo = { fg = c.info }
  h.DiagnosticHint = { fg = c.fg_muted }
  h.DiagnosticOk = { fg = c.ok }
  h.DiagnosticVirtualTextError = { fg = c.err, bg = c.diag_err_bg }
  h.DiagnosticVirtualTextWarn = { fg = c.warn, bg = c.diag_warn_bg }
  h.DiagnosticVirtualTextInfo = { fg = c.info, bg = c.diag_info_bg }
  h.DiagnosticVirtualTextHint = { fg = c.fg_muted, bg = c.diag_hint_bg }
  h.DiagnosticVirtualTextOk = { fg = c.ok, bg = c.diag_ok_bg }
  h.DiagnosticUnderlineError = { sp = c.err, undercurl = true }
  h.DiagnosticUnderlineWarn = { sp = c.warn, undercurl = true }
  h.DiagnosticUnderlineInfo = { sp = c.info, undercurl = true }
  h.DiagnosticUnderlineHint = { sp = c.fg_muted, undercurl = true }
  h.DiagnosticUnderlineOk = { sp = c.ok, undercurl = true }
  h.DiagnosticFloatingError = { fg = c.err, bg = c.surface }
  h.DiagnosticFloatingWarn = { fg = c.warn, bg = c.surface }
  h.DiagnosticFloatingInfo = { fg = c.info, bg = c.surface }
  h.DiagnosticFloatingHint = { fg = c.fg_muted, bg = c.surface }
  h.DiagnosticFloatingOk = { fg = c.ok, bg = c.surface }
  h.DiagnosticSignError = { fg = c.err }
  h.DiagnosticSignWarn = { fg = c.warn }
  h.DiagnosticSignInfo = { fg = c.info }
  h.DiagnosticSignHint = { fg = c.fg_muted }
  h.DiagnosticSignOk = { fg = c.ok }
  h.DiagnosticDeprecated = { sp = c.fg_muted, strikethrough = true }
  h.DiagnosticUnnecessary = { fg = c.syn_comment }

  -- ── plugins: gitsigns / mini.diff ────────────────────────────────────────
  h.GitSignsAdd = { fg = c.ok }
  h.GitSignsChange = { fg = c.info }
  h.GitSignsDelete = { fg = c.err }
  h.GitSignsTopdelete = { fg = c.err }
  h.GitSignsChangedelete = { fg = c.warn }
  h.GitSignsUntracked = { fg = c.fg_muted }
  h.GitSignsAddInline = { bg = c.diff_text }
  h.GitSignsDeleteInline = { bg = c.diff_delete }
  h.GitSignsCurrentLineBlame = { fg = c.syn_comment }
  h.MiniDiffSignAdd = { fg = c.ok }
  h.MiniDiffSignChange = { fg = c.info }
  h.MiniDiffSignDelete = { fg = c.err }

  -- telescope / fzf-lua / snacks picker
  h.TelescopeNormal = { fg = c.fg, bg = c.surface }
  h.TelescopeBorder = { fg = c.frame, bg = c.surface }
  h.TelescopeTitle = { fg = c.fg, bold = true }
  h.TelescopePromptNormal = { fg = c.fg, bg = c.surface_2 }
  h.TelescopePromptBorder = { fg = c.frame, bg = c.surface_2 }
  h.TelescopePromptPrefix = { fg = c.accent }
  h.TelescopePromptCounter = { fg = c.fg_muted }
  h.TelescopeResultsNormal = { fg = c.fg_muted, bg = c.surface }
  h.TelescopePreviewNormal = { fg = c.fg, bg = c.surface }
  h.TelescopeSelection = { fg = c.fg, bg = c.line }
  h.TelescopeSelectionCaret = { fg = c.accent, bg = c.line }
  h.TelescopeMatching = { fg = c.accent, bold = true }
  h.TelescopeMultiSelection = { fg = c.accent }
  h.FzfLuaNormal = { link = "NormalFloat" }
  h.FzfLuaBorder = { link = "FloatBorder" }
  h.FzfLuaTitle = { link = "FloatTitle" }
  h.FzfLuaCursorLine = { link = "CursorLine" }
  h.SnacksPickerMatch = { fg = c.accent, bold = true }

  -- nvim-cmp / blink.cmp
  h.CmpItemAbbr = { fg = c.fg }
  h.CmpItemAbbrDeprecated = { fg = c.syn_comment, strikethrough = true }
  h.CmpItemAbbrMatch = { fg = c.accent, bold = true }
  h.CmpItemAbbrMatchFuzzy = { fg = c.accent }
  h.CmpItemMenu = { fg = c.fg_muted }
  h.CmpItemKind = { fg = c.fg_muted }
  h.CmpGhostText = { fg = c.syn_comment }
  h.BlinkCmpMenu = { link = "Pmenu" }
  h.BlinkCmpMenuBorder = { link = "FloatBorder" }
  h.BlinkCmpMenuSelection = { link = "PmenuSel" }
  h.BlinkCmpLabel = { fg = c.fg }
  h.BlinkCmpLabelMatch = { fg = c.accent, bold = true }
  h.BlinkCmpLabelDetail = { fg = c.fg_muted }
  h.BlinkCmpLabelDeprecated = { fg = c.syn_comment, strikethrough = true }
  h.BlinkCmpKind = { fg = c.fg_muted }
  h.BlinkCmpDoc = { link = "NormalFloat" }
  h.BlinkCmpDocBorder = { link = "FloatBorder" }
  h.BlinkCmpGhostText = { fg = c.syn_comment }

  -- file trees: nvim-tree / neo-tree / oil
  h.NvimTreeNormal = { fg = c.fg, bg = c.surface }
  h.NvimTreeNormalNC = { fg = c.fg, bg = c.surface }
  h.NvimTreeWinSeparator = { fg = c.frame, bg = c.surface }
  h.NvimTreeRootFolder = { fg = c.accent, bold = true }
  h.NvimTreeFolderName = { fg = c.fg }
  h.NvimTreeOpenedFolderName = { fg = c.accent }
  h.NvimTreeFolderIcon = { fg = c.fg_muted }
  h.NvimTreeIndentMarker = { fg = c.line }
  h.NvimTreeGitDirty = { fg = c.info }
  h.NvimTreeGitNew = { fg = c.ok }
  h.NvimTreeGitDeleted = { fg = c.err }
  h.NeoTreeNormal = { fg = c.fg, bg = c.surface }
  h.NeoTreeNormalNC = { fg = c.fg, bg = c.surface }
  h.NeoTreeWinSeparator = { fg = c.frame, bg = c.surface }
  h.NeoTreeRootName = { fg = c.accent, bold = true }
  h.NeoTreeDirectoryName = { fg = c.fg }
  h.NeoTreeDirectoryIcon = { fg = c.fg_muted }
  h.NeoTreeIndentMarker = { fg = c.line }
  h.NeoTreeGitAdded = { fg = c.ok }
  h.NeoTreeGitModified = { fg = c.info }
  h.NeoTreeGitDeleted = { fg = c.err }
  h.NeoTreeGitUntracked = { fg = c.fg_muted }
  h.NeoTreeGitConflict = { fg = c.warn }
  h.OilDir = { fg = c.accent }
  h.OilDirIcon = { fg = c.fg_muted }

  -- indent-blankline / which-key / flash / lazy / noice / trouble / mini.statusline
  h.IblIndent = { fg = c.line }
  h.IblScope = { fg = c.frame }
  h.IndentBlanklineChar = { fg = c.line }
  h.WhichKey = { fg = c.accent }
  h.WhichKeyGroup = { fg = c.fg }
  h.WhichKeyDesc = { fg = c.fg_muted }
  h.WhichKeySeparator = { fg = c.syn_comment }
  h.WhichKeyValue = { fg = c.fg_muted }
  h.WhichKeyBorder = { link = "FloatBorder" }
  h.FlashLabel = { fg = c.on_accent, bg = c.accent, bold = true }
  h.FlashMatch = { fg = c.fg, bg = c.search }
  h.FlashCurrent = { fg = c.on_accent, bg = c.accent }
  h.FlashBackdrop = { fg = c.syn_comment }
  h.LazyH1 = { fg = c.on_accent, bg = c.accent, bold = true }
  h.LazyButton = { fg = c.fg, bg = c.line }
  h.LazyButtonActive = { fg = c.on_accent, bg = c.accent }
  h.LazyCommit = { fg = c.ok }
  h.LazyReasonPlugin = { fg = c.info }
  h.LazySpecial = { fg = c.accent }
  h.NoiceCmdlinePopup = { fg = c.fg, bg = c.surface }
  h.NoiceCmdlinePopupBorder = { fg = c.accent, bg = c.surface }
  h.NoiceCmdlineIcon = { fg = c.accent }
  h.NoicePopupmenu = { link = "Pmenu" }
  h.NoicePopupmenuSelected = { link = "PmenuSel" }
  h.NotifyBackground = { bg = c.surface }
  h.NotifyERRORBorder = { fg = c.err }
  h.NotifyWARNBorder = { fg = c.warn }
  h.NotifyINFOBorder = { fg = c.info }
  h.NotifyDEBUGBorder = { fg = c.fg_muted }
  h.NotifyTRACEBorder = { fg = c.fg_muted }
  h.NotifyERRORTitle = { fg = c.err }
  h.NotifyWARNTitle = { fg = c.warn }
  h.NotifyINFOTitle = { fg = c.info }
  h.TroubleNormal = { fg = c.fg, bg = c.surface }
  h.TroubleText = { fg = c.fg_muted }
  h.TroubleCount = { fg = c.on_accent, bg = c.accent }
  h.MiniStatuslineModeNormal = { fg = c.on_accent, bg = c.accent, bold = true }
  h.MiniStatuslineModeInsert = { fg = c.on_accent, bg = c.ok, bold = true }
  h.MiniStatuslineModeVisual = { fg = c.on_accent, bg = c.info, bold = true }
  h.MiniStatuslineModeReplace = { fg = c.on_accent, bg = c.err, bold = true }
  h.MiniStatuslineModeCommand = { fg = c.on_accent, bg = c.warn, bold = true }
  h.MiniStatuslineFilename = { fg = c.fg, bg = c.surface }
  h.MiniStatuslineFileinfo = { fg = c.fg_muted, bg = c.surface }
  h.MiniStatuslineDevinfo = { fg = c.fg_muted, bg = c.line }
  h.MiniStatuslineInactive = { fg = c.fg_muted, bg = c.surface }
  h.MiniIndentscopeSymbol = { fg = c.frame }
  h.MiniCursorword = { bg = c.line }
  h.MiniTrailspace = { bg = c.err }

  return h
end

-- lualine theme: `require("lualine").setup({ options = { theme = require(...).lualine } })`
M.lualine = (function()
  local c = M.colors
  local function mode(bg)
    return {
      a = { fg = c.on_accent, bg = bg, gui = "bold" },
      b = { fg = c.fg, bg = c.line },
      c = { fg = c.fg_muted, bg = c.surface },
    }
  end
  return {
    normal = mode(c.accent),
    insert = mode(c.ok),
    visual = mode(c.info),
    replace = mode(c.err),
    command = mode(c.warn),
    terminal = mode(c.ok),
    inactive = {
      a = { fg = c.fg_muted, bg = c.surface },
      b = { fg = c.fg_muted, bg = c.surface },
      c = { fg = c.fg_muted, bg = c.surface },
    },
  }
end)()

--- Set every highlight group and the 16 terminal colours.
function M.apply()
  if vim.g.colors_name then
    vim.cmd("highlight clear")
  end
  if vim.fn.exists("syntax_on") == 1 then
    vim.cmd("syntax reset")
  end
  vim.o.termguicolors = true
  vim.o.background = "dark"
  vim.g.colors_name = "ashd-" .. M.name
  for group, spec in pairs(M.highlights()) do
    vim.api.nvim_set_hl(0, group, spec)
  end
  for i, col in ipairs(M.ansi) do
    vim.g["terminal_color_" .. (i - 1)] = col
  end
  return M
end

M.setup = M.apply
M.load = M.apply

return M
