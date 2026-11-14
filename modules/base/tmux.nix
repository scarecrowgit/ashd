{ config, lib, ... }:

let
  ashdShell = lib.attrByPath [ "ashd" "shell" "enable" ] false config;
in
{
  home-manager.sharedModules = [
    (_: {
      programs.tmux = {
        enable = true;
        terminal = "tmux-256color";
        historyLimit = 100000;
        escapeTime = 10;
        baseIndex = 1;
        mouse = true;
        clock24 = true;
        extraConfig = ''
          set -as terminal-features ",xterm-kitty:RGB"
          set -g set-clipboard on
          set -g renumber-windows on
        '';
      };

      # Start (or reuse) a detached tmux session running `claude remote-control` for the
      # current directory, so it survives closing the shell. Implemented by the ashd CLI
      # (was the bash script `claude-rc`); `ashd` is on PATH only when ashd.shell.enable.
      programs.zsh.shellAliases = lib.mkIf ashdShell { claude-rc = "ashd claude-rc"; };
    })
  ];
}
