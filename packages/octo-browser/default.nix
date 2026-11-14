{ lib, stdenv, fetchurl, gnutar, copyDesktopItems, makeDesktopItem
, autoPatchelfHook, makeWrapper
, alsa-lib, at-spi2-atk, at-spi2-core, atk, cairo, cups, dbus, expat
, fontconfig, freetype, gdk-pixbuf, glib, gtk3, harfbuzz, libdrm, libglvnd
, libnotify, libpulseaudio, libxcb, libX11, libXcomposite, libXcursor
, libXdamage, libXext, libXfixes, libXi, libXrandr, libXrender
, libXScrnSaver, libXtst, libxkbcommon, mesa, nss, nspr, pango
, pipewire, libva, systemd, zlib
, util-linux, libkrb5, curl, icu, libpng, libjpeg_turbo, openssl
, sqlite, bzip2, snappy, libvpx, brotli, zstd, wayland, libxshmfence
, libappindicator-gtk3, libthai, pciutils
}:

let
  pname = "octo-browser";
  version = "latest";
  src = fetchurl {
    url = "https://binaries.octobrowser.org/releases/installer/OctoBrowser.linux.tar.gz";
    hash = "sha256-HItEW8fdPw/OfetWuApG0XK7wv52IWWxEsgbk3a2sYM=";
  };
  desktopItem = makeDesktopItem {
    name = pname;
    exec = "octo-browser %U";
    desktopName = "Octo Browser";
    comment = "Anti-detect browser for multi-accounting";
    categories = [ "Network" "WebBrowser" ];
  };
  runtimeLibs = [
    alsa-lib at-spi2-atk at-spi2-core atk cairo cups dbus expat
    fontconfig freetype gdk-pixbuf glib gtk3 harfbuzz libdrm libglvnd
    libnotify libpulseaudio libxcb libX11 libXcomposite libXcursor
    libXdamage libXext libXfixes libXi libXrandr libXrender
    libXScrnSaver libXtst libxkbcommon mesa nss nspr pango
    pipewire libva systemd zlib stdenv.cc.cc
    util-linux libkrb5 curl icu libpng libjpeg_turbo openssl
    sqlite bzip2 snappy libvpx brotli zstd wayland libxshmfence
    libappindicator-gtk3 libthai pciutils
  ];
in
stdenv.mkDerivation {
  inherit pname version;

  src = src;

  nativeBuildInputs = [ gnutar copyDesktopItems autoPatchelfHook makeWrapper ];

  buildInputs = runtimeLibs;

  sourceRoot = ".";

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    mkdir -p $out/bin $out/libexec
    install -Dm755 OctoBrowser.AppImage $out/libexec/octo-browser/octo-browser

    wrapProgram $out/libexec/octo-browser/octo-browser \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs} \
      --set LIBGL_DRIVERS_PATH /run/opengl-driver/lib/dri \
      --set QT_QPA_PLATFORM xcb

    makeWrapper $out/libexec/octo-browser/octo-browser $out/bin/octo-browser

    install -Dm644 -t $out/share/applications ${desktopItem}/share/applications/*
  '';

  meta = {
    description = "Anti-detect browser for multi-accounting";
    homepage = "https://octobrowser.net";
    license = lib.licenses.unfree;
    mainProgram = "octo-browser";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
  };
}
