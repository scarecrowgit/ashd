# xdg-desktop-portal for niri: gnome (screencast, screenshots) + gtk, keyring secrets,
# and fm as the file chooser through xdg-desktop-portal-termfilechooser once the ashd
# CLI (which implements the chooser command) is part of the system.
{ config, lib, pkgs, ... }:

let
  cfg = config.ashd;
  # The CLI is what turns the portal's six arguments into an `fm --pick` command line.
  termFileChooser = cfg.shell.enable && cfg.fm.enable && cfg.apps.fileManager == "fm";

  chooserCmd = pkgs.writeShellScript "fm-file-chooser" ''
    exec ${cfg.package}/bin/ashd sys fm-pick "$@"
  '';
in
{
  config = lib.mkIf cfg.enable {
    programs.niri.useNautilus = false;

    xdg.portal = {
      enable = true;
      extraPortals = [
        pkgs.xdg-desktop-portal-gnome
        pkgs.xdg-desktop-portal-gtk
      ] ++ lib.optional termFileChooser pkgs.xdg-desktop-portal-termfilechooser;

      # Replaces the set the nixpkgs niri module ships (mkForce on the whole entry).
      config.niri = lib.mkForce {
        default = [ "gnome" "gtk" ];
        "org.freedesktop.impl.portal.Access" = "gtk";
        "org.freedesktop.impl.portal.FileChooser" = if termFileChooser then "termfilechooser" else "gtk";
        "org.freedesktop.impl.portal.Notification" = "gtk";
        "org.freedesktop.impl.portal.Secret" = "gnome-keyring";
      };
    };

    services.gnome.gnome-keyring.enable = true;
    security.polkit.enable = true;
    programs.dconf.enable = true;

    # termfilechooser reads its config from $XDG_CONFIG_HOME (not /etc).
    home-manager.sharedModules = lib.mkIf termFileChooser [
      {
        xdg.configFile."xdg-desktop-portal-termfilechooser/config".text = ''
          [filechooser]
          cmd=${chooserCmd}
          default_dir=$HOME
          create_help_file=0
          open_mode=last
          save_mode=last
        '';
      }
    ];
  };
}
