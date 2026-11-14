{
  stdenv,
  fetchurl,
  dpkg,
  autoPatchelfHook,
  wrapGAppsHook3,
  makeShellWrapper,
  gtk3,
  libepoxy,
  libayatana-appindicator,
  keybinder3,
}:

# Repackaged from the release .deb: the AppImage splits the Flutter bundle into
# usr/bin + usr/lib, so the engine can't find <exe dir>/lib/libapp.so outside it
stdenv.mkDerivation rec {
  pname = "flclashx";
  version = "0.4.2";

  src = fetchurl {
    url = "https://github.com/pluralplay/FlClashX/releases/download/v${version}/FlClashX-linux-amd64.deb";
    hash = "sha256-whT1GPvJ/ZRoZtNSifX2y6BvHDswZyidYyrVo5rFGIA=";
  };

  nativeBuildInputs = [
    dpkg
    autoPatchelfHook
    wrapGAppsHook3
    makeShellWrapper
  ];

  buildInputs = [
    gtk3
    libepoxy
    libayatana-appindicator
    keybinder3
  ];

  installPhase = ''
    runHook preInstall

    mkdir -p $out
    cp -r opt usr/share $out/

    substituteInPlace $out/share/applications/com.follow.clashx.desktop \
      --replace-fail 'Exec=/opt/FlClashX/FlClashX' 'Exec=${pname}'

    runHook postInstall
  '';

  dontWrapGApps = true;

  # a wrapper, not a symlink: Flutter resolves lib/ and data/ from /proc/self/exe.
  # TUN needs FlClashCore setuid-root next to the executable, impossible in the
  # store, so prefer the copy modules/desktop/flclashx.nix keeps in tunDir
  postFixup = ''
    makeShellWrapper $out/opt/FlClashX/FlClashX $out/bin/${pname} "''${gappsWrapperArgs[@]}" \
      --run 'if [ "$(cat ${passthru.tunDir}/.source 2>/dev/null)" = '"$out/opt/FlClashX"' ]; then exec ${passthru.tunDir}/FlClashX "$@"; fi'
  '';

  passthru.tunDir = "/var/lib/flclashx";

  meta.platforms = [ "x86_64-linux" ];
}
