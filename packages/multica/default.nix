{ lib, appimageTools, fetchurl }:

let
  pname = "multica";
  version = "0.8.1";

  src = fetchurl {
    url = "https://github.com/multica-ai/multica/releases/download/v${version}/multica-desktop-${version}-linux-x86_64.AppImage";
    hash = "sha256-P1oP8T42J3sY8MQcKOUVebCV+jn1gSe3MWbCR4ieCqM=";
  };

  contents = appimageTools.extract { inherit pname version src; };
in
appimageTools.wrapType2 {
  inherit pname version src;

  extraInstallCommands = ''
    install -Dm644 ${contents}/multica-desktop.desktop $out/share/applications/multica-desktop.desktop
    # Icon= by file path: the dock otherwise upscales a small theme icon
    substituteInPlace $out/share/applications/multica-desktop.desktop \
      --replace-fail 'Exec=AppRun' 'Exec=${pname}' \
      --replace-fail 'Icon=multica-desktop' "Icon=$out/share/icons/hicolor/512x512/apps/multica-desktop.png"
    cp -r ${contents}/usr/share/icons $out/share/
  '';

  meta = {
    description = "Desktop app for Multica, a managed platform for AI coding agents";
    homepage = "https://github.com/multica-ai/multica";
    license = lib.licenses.mit;
    mainProgram = pname;
    platforms = [ "x86_64-linux" ];
    sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
  };
}
