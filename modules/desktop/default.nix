# The ashd desktop: niri fork + custom Rust shell. Declares the `ashd.*` options
# that the host sets from hostVars; every other file in this directory reads them.
{ config, lib, pkgs, inputs, ... }:

let
  inherit (lib) mkOption mkEnableOption types;
in
{
  imports = [
    ./niri.nix
    ./shell.nix
    ./session.nix
    ./lock.nix
    ./portals.nix
    ./audio.nix
    ./env.nix
    ./theme.nix
    ./power.nix
  ];

  config = lib.mkIf config.ashd.enable {
    assertions = [{
      assertion = lib.all (id: lib.hasSuffix ".desktop" id) config.ashd.dock.pinned;
      message = "ashd.dock.pinned entries are desktop file ids and must end in .desktop: ${lib.concatStringsSep ", " config.ashd.dock.pinned}";
    }];
  };

  options.ashd = {
    enable = mkEnableOption "the ashd desktop (niri fork and ashd shell)";

    theme = mkOption {
      type = types.enum [ "ash" "ink" "midnight" "moss" "dusk" ];
      default = "ash";
      description = "Colour theme; written to /etc/ashd/theme for `ashd theme apply`.";
    };

    shell = {
      enable = mkEnableOption "the ashd shell (ashd-shell service, ashd CLI binds, lock screen)";
    };

    package = mkOption {
      type = types.package;
      default = pkgs.ashd;
      defaultText = lib.literalExpression "pkgs.ashd";
      description = "The ashd workspace: bin/ashd, bin/ashd-shell, bin/ashd-lock.";
    };

    niriPackage = mkOption {
      type = types.package;
      default = inputs.niri.packages.${pkgs.stdenv.hostPlatform.system}.niri;
      defaultText = lib.literalExpression "inputs.niri.packages.\${system}.niri";
      description = "The niri fork.";
    };

    input = {
      xkb = {
        layout = mkOption {
          type = types.str;
          default = "us";
          description = "XKB layout(s), comma-separated.";
        };
        options = mkOption {
          type = types.str;
          default = "";
          description = "XKB options.";
        };
      };
      mouse = {
        accelProfile = mkOption {
          type = types.str;
          default = "flat";
          description = "libinput acceleration profile for mice.";
        };
        naturalScroll = mkOption {
          type = types.bool;
          default = false;
          description = "Natural scrolling for mice.";
        };
      };
      focusFollowsMouse = mkOption {
        type = types.bool;
        default = false;
        description = "Focus windows when the pointer hovers over them (only fully visible ones).";
      };
    };

    outputs = mkOption {
      type = types.attrsOf (types.submodule {
        options = {
          mode = mkOption {
            type = types.str;
            example = "1920x1080@180";
            description = "Mode string, WIDTHxHEIGHT[@REFRESH].";
          };
          scale = mkOption {
            type = types.nullOr types.float;
            default = null;
            description = "Output scale; null leaves niri's automatic choice.";
          };
        };
      });
      default = { };
      description = "Outputs by connector name.";
    };

    cursor = {
      name = mkOption {
        type = types.str;
        default = "Bibata-Modern-Ice";
        description = "XCursor theme name (package: bibata-cursors).";
      };
      size = mkOption {
        type = types.int;
        default = 24;
        description = "XCursor size.";
      };
    };

    layout.topStrut = mkOption {
      type = types.numbers.nonnegative;
      default = 30;
      example = 0;
      description = ''
        Space (logical px) reserved at the top of the working area for the shell's strip
        header. Written as `struts { top }` in the static niri config, which is what applies
        until the first `ashd theme apply` has written the generated theme fragment. Once a
        fragment exists it decides the top strut: ashd-tokens' `niri_kdl` writes
        `struts { top }` for every theme (30 px for the header, plus the theme gap under the
        floating themes Ink, Midnight, Moss and Dusk, which start the main area one gap
        below the screen edge), and this option has no effect any more. Set it to 0 only
        without the shell.
      '';
    };

    scratchpad = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Declare the hidden scratchpad workspace that `ashd scratch` and Super+H use.
          The fork's workspace indices count visible workspaces only, so it does not
          shift the Super+1..9 binds (`focus-workspace 1..9`).
        '';
      };
      name = mkOption {
        type = types.str;
        default = "scratch";
        description = "Name of the hidden workspace (`workspace \"name\" { hidden }`). The shell finds it by its hidden flag, not by name.";
      };
    };

    windowRules.noTitleBar = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "^org\\.gnome\\.Nautilus$" "^com\\.mitchellh\\.ghostty$" ];
      description = ''
        app-id regexes of windows that get no compositor-drawn title strip, for apps with
        their own header bar (`window-rule { title-bar false }`). Empty: every window gets
        the strip. `prefer-no-csd` stays on, so most apps drop their own decorations.
      '';
    };

    fm.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Install fm and register it as folder handler and FileManager1 service. Turn off if packages/fm does not build.";
    };

    dock = {
      pinned = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "obsidian.desktop" "thunderbird.desktop" ];
        description = ''
          Desktop file ids pinned to the dock after the terminal, browser and file manager
          (those three always come first, derived from `ashd.apps`). Written to
          /etc/ashd/shell.json.
        '';
      };
    };

    apps = {
      terminal = mkOption {
        type = types.str;
        default = "kitty";
        description = "Terminal command (Super+Return); `<command>.desktop` is its dock entry.";
      };
      browser = mkOption {
        type = types.str;
        default = "zen-twilight";
        description = "Browser command (Super+B); `<command>.desktop` is its dock entry.";
      };
      fileManager = mkOption {
        type = types.str;
        default = "fm";
        description = "File manager command (Super+E); `<command>.desktop` is its dock entry.";
      };
    };
  };
}
