-- Monochrome (ported from wh1tepearl/dotfiles foot.ini palette)
local colors = {
  -- Base colors
  bg = "#19120e",
  fg = "#c5c3c2",

  -- ANSI colors (foot regular0-7)
  black   = "#19120e",
  red     = "#978479",
  green   = "#AC8B7A",
  yellow  = "#CB9779",
  blue    = "#AF9789",
  magenta = "#ACA097",
  cyan    = "#B7A699",
  white   = "#c5c3c2",

  -- Bright colors (foot bright0-7)
  bright_black   = "#6f615b",
  bright_red     = "#978479",
  bright_green   = "#AC8B7A",
  bright_yellow  = "#CB9779",
  bright_blue    = "#AF9789",
  bright_magenta = "#ACA097",
  bright_cyan    = "#B7A699",
  bright_white   = "#c5c3c2",

  -- UI colors
  cursor      = "#c5c3c2",
  cursor_text = "#19120e",
  selection_bg = "#444444",
  selection_fg = "#c5c3c2",

  -- Derived UI
  bg_darker = "#0e0b08",
  bg_lighter = "#241a14",
  bg_float   = "#1c1510",
  bg_popup   = "#201610",

  -- Additional UI elements
  border      = "#444444",
  line_number = "#6f615b",
  comment     = "#6f615b",

  -- Git colors
  git_add    = "#B7A699",
  git_change = "#CB9779",
  git_delete = "#978479",

  -- Diagnostics
  error   = "#978479",
  warning = "#CB9779",
  info    = "#AF9789",
  hint    = "#B7A699",

  -- Accent
  accent = "#c9b890",

  none = "NONE",
}

return colors
