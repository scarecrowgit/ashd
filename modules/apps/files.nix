# Files: the fm file manager (packages/fm) as folder handler and
# org.freedesktop.FileManager1 service, plus the gvfs/tumbler/xarchiver plumbing.
# The termfilechooser file-dialog wiring is in modules/desktop/portals.nix.
{ config, lib, pkgs, ... }:

let
  cfg = config.ashd;
in
{
  config = lib.mkMerge [
    {
      services.gvfs.enable = true; # mounts, trash, MTP
      services.tumbler.enable = true; # thumbnails
      environment.systemPackages = [ pkgs.xarchiver ]; # archive extraction helper
    }

    (lib.mkIf (cfg.enable && cfg.fm.enable) {
      environment.systemPackages = [ pkgs.fm ];
      # "Show in folder" from browsers and chat apps: packages/fm ships
      # share/dbus-1/services/org.freedesktop.FileManager1.service
      services.dbus.packages = [ pkgs.fm ];

      # System-wide default; a user-level mimeapps.list still wins.
      xdg.mime.defaultApplications."inode/directory" = "fm.desktop";
    })

    # fm takes its theme from `--theme`; it has no generated theme file. With the
    # shell enabled, a higher-priority fm.desktop launches it through `ashd fm`,
    # which passes the theme currently set in the shell (the dock, the launcher
    # and "open folder" all go through this entry).
    (lib.mkIf (cfg.enable && cfg.fm.enable && cfg.shell.enable) {
      environment.systemPackages = [
        (lib.hiPrio (pkgs.makeDesktopItem {
          name = "fm";
          desktopName = "fm";
          genericName = "File Manager";
          exec = "ashd fm -- %U";
          icon = "fm";
          categories = [ "System" "FileTools" "FileManager" ];
          mimeTypes = [ "inode/directory" ];
          startupWMClass = "fm";
          startupNotify = true;
        }))
      ];
    })
  ];
}
