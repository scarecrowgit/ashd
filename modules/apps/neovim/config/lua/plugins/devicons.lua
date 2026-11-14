return {
  -- Monochrome file icons: everything in the Ash accent colour
  {
    "nvim-tree/nvim-web-devicons",
    opts = function()
      return {
        -- With color_icons off, every icon uses the default icon's color
        color_icons = false,
        default = true,
        override = {
          default_icon = { icon = "", color = require("ashd.palette").accent, name = "Default" },
        },
      }
    end,
  },
}
