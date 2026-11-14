{ pkgs, lib, ... }:

{
  # For debugging and troubleshooting Secure Boot.
  environment.systemPackages = [ pkgs.sbctl ];

  boot.loader = {
    # Lanzaboote replaces systemd-boot; GRUB must be off (only one bootloader).
    systemd-boot.enable = lib.mkForce false;
    grub.enable = lib.mkForce false;
    efi.canTouchEfiVariables = true;
    efi.efiSysMountPoint = "/boot";
    timeout = 15; # Auto-boot default entry after 15s if nothing is selected
  };

  boot.lanzaboote = {
    enable = true;
    pkiBundle = "/var/lib/sbctl";
    configurationLimit = 10;
  };
}
