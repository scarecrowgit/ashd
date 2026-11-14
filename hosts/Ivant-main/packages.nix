{ pkgs, inputs, ... }:

let
  flakePkgs = inputs.self.packages.${pkgs.stdenv.hostPlatform.system};
in

{
  environment.systemPackages = with pkgs; [
    # Communication
    thunderbird
    mattermost-desktop
    ayugram-desktop
    discord

    # Desktop apps
    obsidian
    morgen
    obs-studio
    spotify
    spicetify-cli
    github-desktop
    proton-vpn
    lmstudio
    zoom-us

    # AI assistants
    opencode
    gemini-cli
    codex

    # Terminal & eye candy
    kitty
    tmux
    fastfetch
    microfetch
    pfetch
    cava

    # DevOps / cloud
    awscli2
    yandex-cloud   # `yc` - Yandex Cloud CLI (S3 backups live in the kz1 region)
    terraform
    kubectl
    kubernetes-helm
    k9s
    argocd
    ansible
    postgresql
    rclone
    bind

    # Development
    nodejs_22
    (python3.withPackages (ps: with ps; [ pyftpdlib ]))
    jetbrains.datagrip

    # System utilities
    blueman
    pavucontrol
    unzip
    zip
    p7zip
    flameshot
    btop
    wireguard-tools
  ] ++ (with flakePkgs; [
    # Anti-detect browsers (built from ./packages)
    octo-browser
    dolphin-anty
    adspower

    # Learning harness (AppImage, built from ./packages)
    learnharness

    # Multica desktop client for AI coding agents (AppImage, built from ./packages)
    multica
  ]);
}
