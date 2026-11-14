# Dev shell for the ashd workspace. Everything (cargo, clippy, tests, the nested test
# session) runs through it:
#
#     nix-shell ashd/shell.nix --run 'cd ashd && cargo check --workspace'
#
# nixpkgs is taken from the repo's flake.lock so the dev shell and the flake build
# (packages/ashd) use the same rustc and the same system libraries. If the lock cannot
# be read, it falls back to <nixpkgs>.
{ pkgs ? null }:
let
  lockFile = ../flake.lock;
  lockedNixpkgs =
    if builtins.pathExists lockFile then
      let
        lock = builtins.fromJSON (builtins.readFile lockFile);
        node = lock.nodes.${lock.nodes.root.inputs.nixpkgs or "nixpkgs"} or null;
        locked = if node == null then null else node.locked or null;
      in
      if locked != null && (locked.type or "") == "github" then
        builtins.fetchTarball {
          url = "https://github.com/${locked.owner}/${locked.repo}/archive/${locked.rev}.tar.gz";
          sha256 = locked.narHash;
        }
      else
        null
    else
      null;
  p =
    if pkgs != null then pkgs
    else if lockedNixpkgs != null then import lockedNixpkgs { }
    else import <nixpkgs> { };

  # Runtime libraries GPUI dlopen()s (wayland, vulkan, xkbcommon) plus what the
  # services link against. Also exported via LD_LIBRARY_PATH for `cargo run`/tests only:
  # binaries must not rely on it leaking to children (apps are spawned through niri).
  libs = with p; [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    fontconfig
    freetype
    openssl
    zstd
    libpulseaudio
    linux-pam
    dbus
    libinput
    systemdLibs # libudev
    libdrm
    mesa
  ];
in
p.mkShell {
  name = "ashd";
  nativeBuildInputs = with p; [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
    pkg-config
    cmake
    clang
    mold
    perl
    git
  ];
  buildInputs = libs;

  LD_LIBRARY_PATH = p.lib.makeLibraryPath libs;
  LIBCLANG_PATH = "${p.libclang.lib}/lib";
  RUST_SRC_PATH = "${p.rustPlatform.rustLibSrc}";
  # mold as the linker (big link times otherwise: gpui + wgpu).
  RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
  # Build output goes to ashd/target (gitignored) unless CARGO_TARGET_DIR is already set,
  # e.g. to keep the ~20 GB of artefacts on another disk.
  # Fonts for headless runs (the nested test session has no user fontconfig).
  FONTCONFIG_FILE = p.makeFontsConf {
    # Theme fonts (tokens.json). Manrope (Dusk) is not in nixpkgs: it falls back to Inter.
    fontDirectories = with p; [ inter jetbrains-mono ibm-plex geist-font dejavu_fonts noto-fonts-color-emoji ];
  };
  # GPU-less machines: export VK_ICD_FILENAMES=${p.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json
  # (lavapipe) before running the nested session. Not needed here (RADV via renderD128).

  shellHook = ''
    # Incremental artefacts grow past 10 GB; the crates are small, so full per-crate
    # rebuilds are cheap enough.
    export CARGO_INCREMENTAL=0
  '';
}
