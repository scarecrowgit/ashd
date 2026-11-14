{ appimageTools, fetchurl, lib }:

let
  pname = "dolphin-anty";
  version = "latest";
  src = fetchurl {
    url = "https://app.dolphin-anty-mirror3.net/anty-app/dolphin-anty-linux-x86_64-latest.AppImage";
    hash = "sha256-zCr8JAMOjv+Qo+KsZGu24HojWSn3mganColVfkiseeM=";
  };
  appimageContents = appimageTools.extractType2 { inherit pname version src; };
in
appimageTools.wrapType2 {
  inherit pname version src;

  extraInstallCommands = ''
    install -Dm644 ${appimageContents}/dolphin_anty.desktop $out/share/applications/dolphin-anty.desktop
    substituteInPlace $out/share/applications/dolphin-anty.desktop \
      --replace-fail 'Exec=AppRun --no-sandbox' 'Exec=${pname} --no-sandbox'
    cp -r ${appimageContents}/usr/share/icons $out/share/
  '';
}
