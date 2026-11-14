# Idle, night light and brightness (ashd-svc-idle / -nightlight / -brightness).
#
# The services work out of the box (idle stages, night light, laptop backlight through
# logind). What needs the system's help is DDC/CI brightness for external monitors:
# it talks I2C through /dev/i2c-*, which needs the i2c-dev module and membership of the
# `i2c` group. That is opt-in (`ashd.brightness.ddc.enable`, default off): some monitors
# misbehave when poked over DDC/CI, so nothing touches the bus until you ask.
#
# Everything the Rust side reads lands in /etc/ashd/power.json (same sections as the
# `idle`, `nightlight` and `brightness` keys of shell.json; see crates/services/*/src/
# config.rs and settings.rs for the schema), e.g.
#
#   ashd.power.settings = {
#     idle = { lockAfter = "10m"; screenOffAfter = "15m"; };       # "never" or 0 disables a stage
#     nightlight = { mode = "sun"; latitude = 52.5; longitude = 13.4; temperature = 3400; };
#   };
{ config, lib, hostVars, ... }:

let
  inherit (lib) mkEnableOption mkIf mkMerge mkOption types;
  cfg = config.ashd;
in
{
  options.ashd = {
    brightness.ddc.enable = mkEnableOption ''
      DDC/CI brightness control for external monitors: loads i2c-dev (`hardware.i2c.enable`)
      and adds the primary user to the `i2c` group. Log out and in (or reboot) after enabling
      it, then check with `ashd osd brightness +0`; to turn it off again set this back to
      false (and the brightness slider of the external monitor disappears)
    '';

    power.settings = mkOption {
      type = types.attrsOf types.anything;
      default = { };
      example = lib.literalExpression ''
        {
          idle.lockAfter = "10m";
          idle.screenOffAfter = "15m";
          nightlight = { mode = "sun"; latitude = 52.5; longitude = 13.4; };
        }
      '';
      description = ''
        Extra sections for /etc/ashd/power.json, merged over shell.json by the idle,
        night-light and brightness services (the user's ~/.config/ashd/shell.json still
        wins). Sections: `idle` (enabled, lockAfter, screenOffAfter), `nightlight` (mode,
        temperature, start, end, latitude, longitude, transitionSecs), `brightness`
        (backlight, ddc).
      '';
    };
  };

  config = mkIf cfg.enable (mkMerge [
    (mkIf cfg.brightness.ddc.enable {
      hardware.i2c.enable = true;
      users.users.${hostVars.userName}.extraGroups = [ config.hardware.i2c.group ];
    })

    (mkIf cfg.shell.enable {
      environment.etc."ashd/power.json".text = builtins.toJSON (lib.recursiveUpdate
        (lib.optionalAttrs cfg.brightness.ddc.enable { brightness.ddc = true; })
        cfg.power.settings);
    })
  ]);
}
