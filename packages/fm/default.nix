# fm: keyboard-first GPUI file manager (flake input `fm-src`, a plain git checkout).
# Replaces packaging/install.sh and the package half of packaging/integrate.sh; the
# system half (default handler, portal, niri rule) lives in modules/apps/files.nix.
{
  lib,
  rustPlatform,
  src,
  pkg-config,
  cmake,
  clang,
  mold,
  makeWrapper,
  wayland,
  libxkbcommon,
  vulkan-loader,
  fontconfig,
  freetype,
  openssl,
  zstd,
  libx11,
  libxcb,
  libgit2,
  libclang,
}:

let
  # shell.nix `libs`: gpui dlopens the first four at runtime, the rest are linked.
  runtimeLibs = [
    wayland
    libxkbcommon
    vulkan-loader
    fontconfig
    freetype
    openssl
    zstd
    libx11
    libxcb
    libgit2
  ];
in
rustPlatform.buildRustPackage {
  pname = "fm";
  version = "0.1.0";
  inherit src;

  # gpui 0.2.2 comes from crates.io; only the xattr patch is a path (vendor/xattr).
  cargoLock.lockFile = "${src}/Cargo.lock";

  nativeBuildInputs = [
    pkg-config
    cmake
    clang
    mold
    makeWrapper
    rustPlatform.bindgenHook
  ];
  buildInputs = runtimeLibs;
  LIBCLANG_PATH = "${libclang.lib}/lib";

  doCheck = false;

  postInstall = ''
    # Same wrapper install.sh generated: the dlopen'd libs are not in any RUNPATH.
    # (fm launches nothing that inherits it except pkexec/xdg-open helpers.)
    wrapProgram $out/bin/fm \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}

    install -Dm644 packaging/fm.desktop $out/share/applications/fm.desktop
    mkdir -p $out/share/icons
    cp -r packaging/hicolor $out/share/icons/hicolor

    # "Show in folder" from browsers and chat apps (org.freedesktop.FileManager1);
    # picked up through services.dbus.packages.
    mkdir -p $out/share/dbus-1/services
    cat > $out/share/dbus-1/services/org.freedesktop.FileManager1.service <<SERVICE
    [D-BUS Service]
    Name=org.freedesktop.FileManager1
    Exec=$out/bin/fm --dbus-service
    SERVICE
  '';

  meta = {
    description = "Keyboard-first file manager for Wayland (GPUI)";
    mainProgram = "fm";
    platforms = [ "x86_64-linux" ];
  };
}
