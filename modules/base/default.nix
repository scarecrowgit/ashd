{ pkgs, ... }:

{
  imports = [
    ./locale.nix
    ./boot.nix
    ./nix.nix
    ./zsh.nix
    ./starship.nix
    ./tmux.nix
    ./gh.nix
    ./hermes.nix
    ./fonts.nix
  ];

  # Enable Chromium SUID sandbox for multi-session browsers
  programs.chromium.enable = true;

  # Provide compatibility for running pre-built Linux binaries (needed by Octo Browser, Dolphin Anty, AdsPower)
  programs.nix-ld.enable = true;
  programs.nix-ld.libraries = with pkgs; [
    stdenv.cc.cc
    zlib
    fuse3
    mesa
    libgbm
    libglvnd
    fontconfig
    freetype
    glib
    nspr
    nss
    atk
    at-spi2-atk
    at-spi2-core
    dbus
    cups
    expat
    libxcb
    libxkbcommon
    alsa-lib
    libX11
    libXext
    cairo
    pango
    systemd
    libXcomposite
    libXdamage
    libXfixes
    libXrandr
    libdrm
    pipewire
    libpulseaudio
    libnotify
    libva
    libappindicator-gtk3
    wayland
    libxshmfence
    util-linux
    libkrb5
    curl
    icu
    libpng
    libjpeg_turbo
    openssl
    sqlite
    bzip2
    snappy
    libvpx
    brotli
    zstd
    pciutils
    libthai
  ];
}
