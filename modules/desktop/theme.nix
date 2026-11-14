# Fonts, GTK, cursor and the dark-mode switch. Colours of the ashd theme itself are
# applied at runtime by `ashd theme apply` (shell.nix); this file is the static part.
{ config, lib, pkgs, ... }:

let
  cfg = config.ashd;

  # Not in nixpkgs any more (manrope was removed upstream) or only inside the
  # 1.5 GB google-fonts tree: take the variable TTFs straight from google/fonts.
  googleRev = "5174b3333331c966c38f4355d50b03ca1c1df2f9";
  googleFont = name: family: file: hash: pkgs.fetchurl {
    inherit name hash;
    url = "https://raw.githubusercontent.com/google/fonts/${googleRev}/ofl/${family}/${file}";
  };
  extraFonts = pkgs.runCommand "ashd-google-fonts" { } ''
    dest=$out/share/fonts/truetype
    mkdir -p $dest
    cp ${googleFont "InterTight.ttf" "intertight" "InterTight%5Bwght%5D.ttf" "sha256-uBtz3LZN88IwyrreffbFdzv4YyM/JMnuUQh1GfH4i28="} $dest/InterTight-Variable.ttf
    cp ${googleFont "InterTight-Italic.ttf" "intertight" "InterTight-Italic%5Bwght%5D.ttf" "sha256-FdPt8MPSVgZYUpuMTHOY0GxzCkaVSExoC2ycqeQ4W+I="} $dest/InterTight-Italic-Variable.ttf
    cp ${googleFont "Manrope.ttf" "manrope" "Manrope%5Bwght%5D.ttf" "sha256-0GOb5F0K8255gXJBnXvRc8S9Tynit2y7adsdEb+LCkA="} $dest/Manrope-Variable.ttf
  '';
in
{
  config = lib.mkIf cfg.enable {
    # All of these cover Latin and Cyrillic (checked with fc-query).
    fonts.packages = [
      pkgs.inter
      pkgs.jetbrains-mono
      pkgs.nerd-fonts.jetbrains-mono
      pkgs.ibm-plex
      pkgs.geist-font # Geist and Geist Mono
      extraFonts # Inter Tight, Manrope
    ];

    fonts.fontconfig.defaultFonts = {
      sansSerif = [ "Inter" ];
      monospace = [ "JetBrainsMono Nerd Font" ];
    };

    environment.systemPackages = [
      pkgs.adw-gtk3
      pkgs.bibata-cursors
    ];

    programs.dconf.enable = true;

    home-manager.sharedModules = [
      ({ config, pkgs, ... }: {
        home.pointerCursor = {
          package = pkgs.bibata-cursors;
          inherit (cfg.cursor) name size;
          gtk.enable = true;
          x11.enable = false;
        };

        gtk = {
          enable = true;
          theme = {
            name = "adw-gtk3-dark";
            package = pkgs.adw-gtk3;
          };
          gtk4.theme = config.gtk.theme;
          gtk3.extraConfig.gtk-application-prefer-dark-theme = true;
          gtk4.extraConfig.gtk-application-prefer-dark-theme = true;
          # `ashd theme apply` writes this file (the activation hook runs before any
          # GTK app starts); until then GTK just warns about the missing import.
          gtk3.extraCss = ''@import url("file://${config.home.homeDirectory}/.local/state/ashd/generated/gtk.css");'';
          gtk4.extraCss = ''@import url("file://${config.home.homeDirectory}/.local/state/ashd/generated/gtk.css");'';
        };

        # xdg-desktop-portal-gtk's Settings backend reports color-scheme from this
        # GSettings key, not from the gtk ini files; without it portal-aware apps (Zen
        # included) get "no preference" and end up with inconsistent light/dark colours.
        dconf.settings."org/gnome/desktop/interface" = {
          color-scheme = "prefer-dark";
        };
      })
    ];

    # Replaces the per-user .hm-bak naming used before; existing hand-written dotfiles
    # (gtk settings.ini, portal config) are moved aside instead of blocking activation.
    home-manager.backupFileExtension = lib.mkDefault "hm-bak";
  };
}
