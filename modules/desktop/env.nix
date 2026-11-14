# Session environment. GDK_BACKEND is deliberately not set globally: it would force
# every GTK app, including X11-only ones started through xwayland-satellite, onto Wayland.
{ config, lib, ... }:

{
  config = lib.mkIf config.ashd.enable {
    environment.sessionVariables = {
      NIXOS_OZONE_WL = "1";
      ELECTRON_OZONE_PLATFORM_HINT = "auto";
      MOZ_ENABLE_WAYLAND = "1";
      QT_QPA_PLATFORM = "wayland;xcb";
    };
    # Octo Browser keeps its own QT_QPA_PLATFORM=xcb (set in packages/octo-browser).
  };
}
