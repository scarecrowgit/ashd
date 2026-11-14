{
  description = "Ivant's NixOS flake: niri fork + ashd shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    zen-browser = {
      url = "github:0xc000022070/zen-browser-flake";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        home-manager.follows = "home-manager";
      };
    };
    betterfox = {
      url = "github:yokoffing/Betterfox";
      flake = false;
    };
    lanzaboote = {
      url = "github:nix-community/lanzaboote/v1.2.0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Official Claude Desktop Linux beta (.deb), repackaged for Nix
    claude-desktop = {
      url = "github:poeck/claude-desktop-nix-flake";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Cargo builds with a shared dependency-artifact derivation (packages/ashd)
    crane.url = "github:ipetkov/crane";

    # The niri fork (exports packages.<system>.niri): canvas mode, hidden workspaces,
    # title strips, view IPC — see ASHD-FORK.md there. Refresh the lock with
    # `nix flake update niri` after every commit on its `ashd` branch. To build
    # against a local checkout instead: --override-input niri git+file:///path/to/niri
    niri = {
      url = "github:scarecrowgit/niri/ashd";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # GPUI file manager, built by packages/fm
    fm-src = {
      url = "github:scarecrowgit/fm";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, ... }@inputs:
    let
      lib = nixpkgs.lib;
      system = "x86_64-linux";
      hostNames = lib.attrNames (lib.filterAttrs (_: type: type == "directory") (builtins.readDir ./hosts));
      mkHost = name:
        let
          hostPath = ./hosts + "/${name}";
          hostVars = import (hostPath + "/vars.nix");
        in
        nixpkgs.lib.nixosSystem {
          specialArgs = {
            inherit inputs hostVars;
          };
          modules = [ hostPath ];
        };

      # Same overlay set the hosts use, so `nix build .#fm` and the system agree.
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [ (import ./overlays/ashd.nix inputs) ];
      };

      hasAshdLock = builtins.pathExists ./ashd/Cargo.lock;
    in
    {
      packages.${system} = {
        niri = inputs.niri.packages.${system}.niri;
        fm = pkgs.fm;
        octo-browser = pkgs.callPackage ./packages/octo-browser { };
        dolphin-anty = pkgs.callPackage ./packages/dolphin-anty { };
        adspower = pkgs.callPackage ./packages/adspower { };
        flclashx = pkgs.callPackage ./packages/flclashx { };
        learnharness = pkgs.callPackage ./packages/learnharness { };
        multica = pkgs.callPackage ./packages/multica { };
      }
      # The workspace lands in ./ashd from another agent; until it has a lockfile
      # there is nothing to build and the attribute stays out of the flake.
      // lib.optionalAttrs hasAshdLock { ashd = pkgs.ashd; };

      devShells.${system} = lib.optionalAttrs (builtins.pathExists ./ashd/shell.nix) {
        default = import ./ashd/shell.nix { inherit pkgs; };
      };

      nixosModules.base = import ./modules/base;

      checks.${system} = {
        vm = import ./tests/vm.nix { inherit pkgs inputs; };
        # The graphical greeter path; cannot pass without GPU acceleration (see tests/vm.nix).
        vm-greeter = import ./tests/vm.nix { inherit pkgs inputs; greeterKind = "ashd"; };
      };

      nixosConfigurations = lib.genAttrs hostNames mkHost;
    };
}
