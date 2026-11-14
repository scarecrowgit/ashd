# Browsers. Zen (twilight) is the default and lives in ./zen-twilight, which also puts
# it on the system PATH (Claude Desktop's xdg-open needs it). Chromium's SUID sandbox
# is enabled in modules/base (programs.chromium.enable) for the anti-detect browsers.
{ pkgs, ... }:

{
  imports = [ ./zen-twilight ];

  environment.systemPackages = [ pkgs.firefox ];
}
