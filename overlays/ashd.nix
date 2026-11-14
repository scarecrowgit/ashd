# Adds the ashd workspace and the fm file manager (built from flake inputs) to pkgs.
# Usage: nixpkgs.overlays = [ (import ../../overlays/ashd.nix inputs) ];
inputs: final: prev: {
  ashd = final.callPackage ../packages/ashd {
    craneLib = inputs.crane.mkLib final;
  };
  fm = final.callPackage ../packages/fm {
    src = inputs.fm-src;
  };
}
