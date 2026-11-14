{ appimageTools, fetchurl }:

let
  pname = "learnharness";
  version = "0.5.0";
  src = fetchurl {
    url = "https://github.com/amosblomqvist/learnharness/releases/download/v${version}/LearnHarness-${version}-x86_64.AppImage";
    hash = "sha256-tb2EmK1TyiqOtwgHNK8OxaquwbtqxV8kboUCnMumncM=";
  };
  appimageContents = appimageTools.extract { inherit pname version src; };
in
appimageTools.wrapType2 {
  inherit pname version src;

  extraInstallCommands = ''
    install -Dm644 ${appimageContents}/learnharness.desktop $out/share/applications/learnharness.desktop
    substituteInPlace $out/share/applications/learnharness.desktop \
      --replace-fail 'Exec=AppRun --no-sandbox' 'Exec=${pname} --no-sandbox'
    cp -r ${appimageContents}/usr/share/icons $out/share/
  '';

  meta.platforms = [ "x86_64-linux" ];
}
