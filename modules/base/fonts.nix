{ pkgs, ... }:

let
  cozette-crossedseven = pkgs.runCommand "cozette-crossedseven" { } ''
    mkdir -p $out/share/fonts/misc $out/share/fonts/truetype
    cp ${./fonts/cozette}/*.otb $out/share/fonts/misc/ 2>/dev/null || true
    cp ${./fonts/cozette}/*.ttf $out/share/fonts/truetype/ 2>/dev/null || true
  '';
in
{
  fonts.enableDefaultPackages = true;
  fonts.packages = [
    cozette-crossedseven
    pkgs.cozette
    pkgs.nerd-fonts.jetbrains-mono
  ];
}
