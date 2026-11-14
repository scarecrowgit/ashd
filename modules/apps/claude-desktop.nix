{ inputs, pkgs, ... }:

{
  imports = [ inputs.claude-desktop.nixosModules.default ];

  programs.claude-desktop.enable = true;
  # Upstream flake lags Anthropic's apt repo; bump the .deb here until it catches up.
  # Latest: curl -s https://downloads.claude.ai/claude-desktop/apt/stable/dists/stable/main/binary-amd64/Packages
  programs.claude-desktop.package =
    inputs.claude-desktop.packages.${pkgs.stdenv.hostPlatform.system}.claude-desktop.overrideAttrs (old: rec {
      version = "5.1231.2";
      src = pkgs.fetchurl {
        url = "https://downloads.claude.ai/claude-desktop/apt/stable/pool/main/c/claude-desktop/claude-desktop_${version}_amd64.deb";
        sha256 = "eb4d16c9aff0a696366d88728f74fa55136be7cfde3c1fe35af4062b45955b6f";
      };
      # 4.x native binding links libpipewire (not in the flake's 2.x dep list)
      buildInputs = (old.buildInputs or [ ]) ++ [ pkgs.pipewire ];
    });
}
