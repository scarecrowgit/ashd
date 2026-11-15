# The ashd shell: user service, CLI on PATH, keybind table for the cheatsheet,
# and the single home-manager activation hook that applies the theme.
{ config, lib, pkgs, ... }:

let
  cfg = config.ashd;
  binds = import ./niri/binds.nix { inherit lib cfg; };

  # What ashd-shell reads at startup (/etc/ashd/shell.json): dock order and the
  # commands behind Super+Return / Super+B / Super+E.
  shellJson = builtins.toJSON {
    dock = lib.unique (map (cmd: "${cmd}.desktop") [ cfg.apps.terminal cfg.apps.browser cfg.apps.fileManager ] ++ cfg.dock.pinned);
    apps = {
      inherit (cfg.apps) terminal browser;
      # `ashd fm` passes the current theme to fm (see modules/apps/files.nix).
      fileManager = if cfg.apps.fileManager == "fm" then "ashd fm" else cfg.apps.fileManager;
    };
    reduceMotion = cfg.shell.reduceMotion;
  };
in
{
  # Polkit authentication agent of the session. "ashd": the shell's built-in agent
  # (ashd-svc-polkit + the polkit dialog panel), switched on by ASHD_POLKIT_AGENT=ashd in
  # the ashd-shell unit; polkit's socket-activated helper (/run/polkit/agent-helper.socket,
  # polkit-agent-helper.socket, set up by security.polkit) does the PAM conversation.
  # "soteria": the interim third-party agent; the built-in one stays off.
  options.ashd.polkitAgent = lib.mkOption {
    type = lib.types.enum [ "ashd" "soteria" ];
    default = "soteria";
    description = ''
      Which polkit authentication agent runs in the ashd session. "soteria" (default
      until the built-in agent has been tested on real hardware) or "ashd" (built into
      ashd-shell; needs ashd.shell.enable).
    '';
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [{
        assertion = cfg.polkitAgent != "ashd" || cfg.shell.enable;
        message = "ashd.polkitAgent = \"ashd\" needs ashd.shell.enable (the agent lives in ashd-shell).";
      }];

      # Exactly one agent per session: soteria (polkit-soteria.service, started with the
      # session) unless the shell's own agent is selected.
      security.soteria.enable = cfg.polkitAgent == "soteria";
      security.polkit.enable = true;

      environment.etc."ashd/theme".text = cfg.theme + "\n";
    }

    (lib.mkIf cfg.shell.enable {
      environment.systemPackages = [ cfg.package ];

      environment.etc."ashd/keybinds.json".text = binds.json;
      environment.etc."ashd/shell.json".text = shellJson;

      systemd.user.services.ashd-shell = {
        description = "ashd desktop shell";
        partOf = [ "graphical-session.target" ];
        after = [ "niri.service" "graphical-session.target" ];
        # The shell launches user apps: it must inherit the PATH niri-session imported
        # into the user manager, not the NixOS default (coreutils only).
        enableDefaultPath = false;
        # WAYLAND_DISPLAY, XDG_*, PATH, DBUS_SESSION_BUS_ADDRESS ... come from the user
        # manager's environment, which niri-session / niri.service import before this
        # unit (ordered After=niri.service) starts.
        # The built-in polkit agent reads this (ashd-svc-polkit, `options_from_env`).
        environment = lib.mkIf (cfg.polkitAgent == "ashd") { ASHD_POLKIT_AGENT = "ashd"; };
        serviceConfig = {
          ExecStart = "${cfg.package}/bin/ashd-shell";
          Restart = "on-failure";
          RestartSec = 1;
          Slice = "session.slice";
        };
      };
      # How niri's docs wire session services (`systemctl --user add-wants niri.service X`).
      systemd.user.services.niri.wants = [ "ashd-shell.service" ];

      home-manager.sharedModules = [
        ({ lib, ... }: {
          # `ashd completions zsh` output is installed with the package; the explicit
          # fpath entry works even where the system site-functions dir is not in fpath.
          programs.zsh.initContent = lib.mkOrder 500 "fpath=(${cfg.package}/share/zsh/site-functions $fpath)";

          home.activation.ashdTheme = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
            run ${cfg.package}/bin/ashd theme apply --quiet
          '';
        })
      ];
    })
  ]);
}
