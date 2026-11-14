{
  hostName = "Ivant-main";
  userName = "vxwwx";
  timeZone = "Asia/Tbilisi";
  locale = "en_GB.UTF-8";
  stateVersion = "25.05";
  videoDrivers = [ "amdgpu" ];
  kbdLayouts = [ "us" "ru" ];
  xkbOptions = "grp:ctrl_space_toggle";
  terminal = "kitty";
  browser = "zen-twilight";
  fileManager = "fm";
  # Connector -> mode. One 1920x1080@180 monitor on DP-2.
  outputs = {
    # No refresh rate on purpose: niri then takes the highest one for this resolution
    # (180.003 Hz here). A rate must match a mode exactly, and "@180" matches nothing,
    # which silently falls back to the preferred 60 Hz mode.
    "DP-2" = { mode = "1920x1080"; };
  };
  tailscale = true;
}
