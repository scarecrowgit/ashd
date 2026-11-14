{ pkgs, ... }:

{
  nix.settings.experimental-features = [ "nix-command" "flakes" ];

  # Deduplicate the store on every build by hard-linking identical files
  nix.settings.auto-optimise-store = true;

  # Automatic garbage collection while idle (frees dead store paths)
  nix.gc.automatic = true;
  nix.gc.dates = "04:15";
  nix.gc.options = "--delete-older-than 7d";

  # Periodic store optimisation service
  nix.optimise.automatic = true;
  nix.optimise.dates = [ "05:15" ];

  nixpkgs.config.allowUnfree = true;

  environment.systemPackages = with pkgs; [
    git
    vim
  ];
}
