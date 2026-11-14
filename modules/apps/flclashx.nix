{ config, lib, pkgs, inputs, ... }:

# FlClashX enables TUN only when <exe dir>/FlClashCore is root-owned and setuid
# (it stat()s the file), so it has to run from a copy outside the read-only store:
# real FlClashX + setuid FlClashCore, lib/ and data/ linked back to the store.
# `ashd sys flclashx-sync` (Rust CLI, runs as root on activation) maintains that copy
# whenever the package changes; see the argument contract below.
let
  flclashx = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.flclashx;
  ashdShell = lib.attrByPath [ "ashd" "shell" "enable" ] false config;
  ashd = config.ashd.package;
in
{
  environment.systemPackages = [ flclashx ];

  # TUN replies (e.g. hijacked DNS from the LAN router) arrive on the tun device
  # while the route back to their source is via the physical NIC; strict rpfilter
  # drops them and nothing resolves
  networking.firewall.checkReversePath = "loose";

  # Without the ashd CLI there is no TUN copy; the package wrapper then falls back to
  # the store binary (TUN unavailable, proxy mode works).
  system.activationScripts.flclashx = lib.mkIf ashdShell ''
    ${ashd}/bin/ashd sys flclashx-sync --src ${flclashx}/opt/FlClashX --dst ${flclashx.tunDir}
  '';
}
