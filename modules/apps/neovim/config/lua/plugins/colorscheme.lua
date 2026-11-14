return {
  -- Local Ash colourscheme (colors/ashd.lua, palette in lua/ashd/palette.lua). Loads
  -- ~/.local/state/ashd/generated/nvim.lua on top of it when `ashd theme apply` made one.
  {
    "ashd-theme",
    dir = vim.fn.stdpath("config") .. "/lua/ashd",
    lazy = false,
    priority = 1000,
    config = function()
      vim.cmd.colorscheme("ashd")
    end,
  },

  -- Monochrome custom theme (ported from wh1tepearl/dotfiles; :colorscheme ghostty)
  {
    "ghostty-theme",
    dir = vim.fn.stdpath("config") .. "/lua/ghostty-theme",
    lazy = true,
    config = function()
      require("ghostty-theme").setup()
    end,
  },

  -- Catppuccin (disabled)
  {
    "catppuccin/nvim",
    name = "catppuccin",
    lazy = true,
    priority = 999,
    opts = {
      flavour = "mocha",
      transparent_background = false,
      term_colors = true,
      integrations = {
        cmp = true,
        gitsigns = true,
        nvimtree = true,
        telescope = true,
        treesitter = true,
        notify = true,
        mini = true,
      },
    },
    config = function(_, opts)
      require("catppuccin").setup(opts)
    end,
  },

  -- Tokyo Night theme (optional alternative)
  {
    "folke/tokyonight.nvim",
    lazy = true,
    priority = 999,
    opts = {
      style = "night", -- storm, moon, night, day
      transparent = false,
      terminal_colors = true,
      styles = {
        comments = { italic = true },
        keywords = { italic = true },
        functions = {},
        variables = {},
      },
    },
  },
}
