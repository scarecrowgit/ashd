{ hostVars, ... }:

{
  ashd = {
    enable = true;
    theme = "ash";
    shell.enable = true;

    input = {
      xkb = {
        layout = builtins.concatStringsSep "," hostVars.kbdLayouts;
        options = hostVars.xkbOptions;
      };
      mouse = {
        accelProfile = "flat";
        naturalScroll = true;
      };
      focusFollowsMouse = true;
    };

    outputs = hostVars.outputs;

    # The graphical login screen (the module default is the text tuigreet). If it ever
    # fails to appear, log in on another console (Ctrl+Alt+F2) and set this back to
    # "tuigreet"; `journalctl -t ashd-greeter` says what it did.
    greeter.kind = "ashd";

    cursor = {
      name = "Bibata-Modern-Ice";
      size = 24;
    };

    # After terminal, browser and file manager (derived from `apps` below).
    dock.pinned = [
      "obsidian.desktop"
      "thunderbird.desktop"
      "discord.desktop"
      "spotify.desktop"
    ];

    apps = {
      terminal = hostVars.terminal;
      browser = hostVars.browser;
      fileManager = hostVars.fileManager;
    };

    # This host deliberately never blanks or sleeps (see default.nix), so the
    # shell's idle stages stay off; lock with Super+Ctrl+Q. Night light starts
    # off as well: turn it on from quick settings.
    power.settings = {
      idle.enabled = false;
      nightlight.mode = "off";
    };
  };
}
