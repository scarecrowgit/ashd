{ hostVars, inputs, pkgs, lib, ... }:

{
  imports = [
    inputs.home-manager.nixosModules.home-manager
    inputs.lanzaboote.nixosModules.lanzaboote
    ../../modules/base
    ../../modules/base/openvpn
    ../../modules/base/docker
    ../../modules/apps
    ../../modules/desktop
    ./hardware-configuration.nix
    ./boot.nix
    ./packages.nix
    ./desktop.nix
    ./storage.nix
  ];

  # Bump Discord past the stale version pinned in nixpkgs (see overlays/discord.nix)
  nixpkgs.overlays = [
    (import ../../overlays/discord.nix)
    (import ../../overlays/ashd.nix inputs)
  ];

  networking.hostName = hostVars.hostName;

  # The LAN router advertises itself as an IPv6 gateway but hands out no prefix,
  # so the RA default route is a black hole: v6 connects hang until they time out.
  # Node-based apps (AdsPower's backend) try v6 first and never fall back in time.
  networking.dhcpcd.extraConfig = "noipv6rs";

  systemd.services.resolvconf.serviceConfig = {
    ExecStartPre = "-${pkgs.coreutils}/bin/rm -f /run/avahi-daemon/pid";
  };

  hardware.graphics.enable = true;
  hardware.bluetooth.enable = true;

  users.users.${hostVars.userName} = {
    isNormalUser = true;
    extraGroups = [ "wheel" "video" "input" "bluetooth" ];
    shell = pkgs.zsh;
  };

  home-manager = {
    useGlobalPkgs = true;
    useUserPackages = true;
    users.${hostVars.userName} = {
      home.stateVersion = hostVars.stateVersion;
    };
  };

  system.stateVersion = hostVars.stateVersion;

  services.openssh = {
    enable = true;
    settings.PasswordAuthentication = true;
  };

  # Ship terminfo for kitty, alacritty, foot and others so SSH sessions from those terminals work.
  environment.enableAllTerminfo = true;

  # Keep the box reachable over SSH: never suspend or hibernate
  systemd.targets = {
    sleep.enable = false;
    suspend.enable = false;
    hibernate.enable = false;
    hybrid-sleep.enable = false;
  };
  services.logind.settings.Login.HandlePowerKey = "poweroff";

  services.tailscale = lib.mkIf (hostVars.tailscale or false) {
    enable = true;
    extraUpFlags = [ "--ssh" ];
    openFirewall = true;
  };

  networking.firewall = lib.mkIf (hostVars.tailscale or false) {
    trustedInterfaces = [ "tailscale0" ];
    allowedUDPPorts = [ 41641 ];
  };
}
