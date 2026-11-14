{ lib, stdenv, fetchurl, dpkg, asar, copyDesktopItems, makeDesktopItem, autoPatchelfHook, makeWrapper
, alsa-lib, at-spi2-atk, at-spi2-core, atk, cairo, cups, dbus, expat
, fontconfig, freetype, gdk-pixbuf, glib, gtk3, harfbuzz, libdrm, libglvnd
, libnotify, libpulseaudio, libxcb, libX11, libXcomposite, libXcursor
, libXdamage, libXext, libXfixes, libXi, libXrandr, libXrender
, libXScrnSaver, libXtst, libxkbcommon, mesa, nss, nspr, pango
, pipewire, libva, systemd, zlib, xdg-utils
, util-linux, libkrb5, curl, icu, libpng, libjpeg_turbo, openssl
, sqlite, bzip2, snappy, libvpx, brotli, zstd, wayland, libxshmfence
, libappindicator-gtk3, libthai, pciutils, vips
}:

let
  pname = "adspower";
  version = "8.6.3";
  src = fetchurl {
    url = "https://version.adspower.net/software/linux-x64-global/${version}/AdsPower-Global-${version}-x64.deb";
    hash = "sha256-GtT/tXILyh+cycYAI733VML7CBLObxg09Yl+OQe+nGM=";
  };
  desktopItem = makeDesktopItem {
    name = pname;
    exec = "adspower %U";
    desktopName = "AdsPower Global";
    comment = "Anti-detect browser for multi-accounting";
    categories = [ "Network" "WebBrowser" ];
    icon = "adspower_global";
    startupWMClass = "adspower_global";
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
    libappindicator-gtk3 libthai pciutils vips
  ];
in
stdenv.mkDerivation {
  inherit pname version;

  src = src;

  nativeBuildInputs = [ dpkg asar copyDesktopItems autoPatchelfHook makeWrapper ];

  buildInputs = runtimeLibs;

  dontConfigure = true;
  dontBuild = true;

  unpackPhase = ''
    dpkg-deb -x $src .
  '';

  installPhase = ''
    mkdir -p $out/bin $out/opt $out/share
    cp -r opt/AdsPower\ Global $out/opt/

    # The Node backend (main.min.js) is forked by the Electron main process, which
    # blanks NODE_OPTIONS, so pass flags via execArgv instead. On networks with a dead
    # IPv6 route, Node's happy-eyeballs gives each address 250ms, drops the slow v4
    # attempt and hangs on v6, so API calls time out and profiles fail to open.
    asar extract "$out/opt/AdsPower Global/resources/app.asar" app
    substituteInPlace app/dist/main.js \
      --replace-fail 'B.fork(Ie,{cwd:Ce,env:a,silent:!0})' \
        'B.fork(Ie,{cwd:Ce,env:a,silent:!0,execArgv:["--no-network-family-autoselection","--dns-result-order=ipv4first"]})'
    rm -r "$out/opt/AdsPower Global/resources/app.asar"{,.unpacked}
    asar pack app "$out/opt/AdsPower Global/resources/app.asar" \
      --unpack-dir '{node_modules/koffi,node_modules/@img/{sharp-darwin-x64,sharp-libvips-darwin-x64,sharp-linuxmusl-x64,sharp-linux-x64}}'
    rm -r app

    # Remove platform-specific .node files for other OSes/ABIs
    find $out/opt \( -name "*.node" -o -name "*.so" \) \( \
      -path "*openbsd*" -o \
      -path "*freebsd*" -o \
      -path "*musl*" -o \
      -path "*linux_ia32*" -o \
      -path "*linux_arm*" -o \
      -path "*linux_riscv*" \
    \) -delete
    # Remove empty directories left behind
    find $out/opt -type d -empty -delete 2>/dev/null || true

    # Create vips compatibility symlink
    ln -sf ${lib.getLib vips}/lib/libvips-cpp.so "$out/opt/AdsPower Global/libvips-cpp.so.8.16.1"

    wrapProgram "$out/opt/AdsPower Global/adspower_global" \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}

    makeWrapper "$out/opt/AdsPower Global/adspower_global" $out/bin/adspower

    install -Dm644 -t $out/share/applications ${desktopItem}/share/applications/*
    cp -r usr/share/icons $out/share/
  '';

  meta = {
    description = "Anti-detect browser for multi-accounting";
    homepage = "https://www.adspower.com";
    license = lib.licenses.unfree;
    mainProgram = "adspower";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
  };
}
