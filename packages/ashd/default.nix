# The ashd Cargo workspace (../../ashd): bin/ashd (CLI), bin/ashd-shell, bin/ashd-lock,
# bin/ashd-greeter.
#
# Built with crane so the (huge) dependency graph, including gpui from the Zed git
# repo, is compiled once in `cargoArtifacts` and reused when only workspace sources
# change. Git dependencies are vendored straight from Cargo.lock.
{
  lib,
  craneLib,
  pkg-config,
  cmake,
  clang,
  mold,
  patchelf,
  installShellFiles,
  libGL,
  wayland,
  libxkbcommon,
  fontconfig,
  freetype,
  vulkan-loader,
  libpulseaudio,
  linux-pam,
  dbus,
  openssl,
  zstd,
  libclang,
}:

let
  root = ../../ashd;

  # cleanCargoSource keeps *.rs/Cargo.*/etc; the workspace also needs its design
  # tokens, embedded assets and vendored crates at build time.
  extraFilter = path: _type:
    builtins.match ".*/(tokens|assets|vendor|fixtures)(/.*)?$" path != null
    # Files pulled in with include_str! from inside a crate's src/.
    || builtins.match ".*\\.lua$" path != null;
  src = lib.cleanSourceWith {
    src = root;
    filter = path: type: (extraFilter path type) || (craneLib.filterCargoSources path type);
    name = "ashd-source";
  };

  # Libraries gpui/winit/wgpu dlopen at run time. They are put in the RUNPATH of the
  # binaries (patchelf below) instead of an LD_LIBRARY_PATH wrapper: the shell
  # launches user apps and the variable would leak into every one of them.
  dlopenLibs = [
    wayland
    libxkbcommon
    vulkan-loader
    fontconfig
    libGL # wgpu's GL fallback (libEGL)
  ];

  commonArgs = {
    pname = "ashd";
    inherit src;
    strictDeps = true;

    nativeBuildInputs = [
      pkg-config
      cmake
      clang
      mold
      patchelf
      installShellFiles
    ];
    buildInputs = [
      wayland
      libxkbcommon
      fontconfig
      freetype
      vulkan-loader
      libpulseaudio
      linux-pam
      dbus
      openssl
      zstd
    ];

    LIBCLANG_PATH = "${libclang.lib}/lib";

    # The workspace's default-members do not list every binary: name them. ashd-greeter
    # (the graphical greetd login screen, modules/desktop/session.nix) is built here.
    cargoExtraArgs = "--locked -p ashd-shell -p ashd-cli -p ashd-lock -p ashd-greeter";
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;

  # Tests (gpui, wayland, PAM) need a display or a session; they run in the dev shell.
  doCheck = false;

  # `ashd` itself dlopens nothing; the shell, the lock screen and the greeter do (the
  # greeter runs as the unprivileged `greeter` user with an empty environment, so it
  # needs the RUNPATH even more than the others).
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath dlopenLibs} \
      $out/bin/ashd-shell $out/bin/ashd-lock $out/bin/ashd-greeter
  '';

  # Completions come from the binary itself (clap_complete).
  postInstall = ''
    installShellCompletion --cmd ashd \
      --bash <($out/bin/ashd completions bash) \
      --zsh <($out/bin/ashd completions zsh) \
      --fish <($out/bin/ashd completions fish)
  '';

  meta = {
    description = "ashd: Wayland desktop shell, lock screen and CLI for the niri fork";
    mainProgram = "ashd";
    platforms = [ "x86_64-linux" ];
  };
})
