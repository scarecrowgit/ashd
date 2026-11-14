{ hostVars, pkgs, ... }:

let
  terminal = hostVars.terminal or "kitty";
in
{
  environment.systemPackages = with pkgs; [
    gcc
    nodejs
    nil
    nixfmt-tree
    ripgrep
    gnumake
    wl-clipboard # clipboard provider for clipboard=unnamedplus
  ];

  home-manager.sharedModules = [
    (_: {
      programs.neovim.enable = true;
      programs.neovim.withRuby = false;
      programs.neovim.withPython3 = false;
      xdg.configFile."nvim".source = ./config;
      xdg.desktopEntries = {
        "nvim" = {
          name = "Neovim wrapper";
          genericName = "Text Editor";
          comment = "Edit text files";
          exec = "${pkgs.${terminal}}/bin/${terminal} --class \"nvim-wrapper\" -e nvim %F";
          icon = "nvim";
          mimeType = [
            "text/plain"
            "text/x-makefile"
          ];
          categories = [
            "Development"
            "TextEditor"
          ];
          terminal = false;
        };
      };
    })
  ];
}
