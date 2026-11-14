{ ... }:

{
  home-manager.sharedModules = [
    ({ pkgs, ... }:
      let
        # Ash accent
        accent = "#E0A458";
        tomlFormat = pkgs.formats.toml { };

        # Styling sections, yazi >= 25.5 schema (mgr/tabs/mode/indicator/pick/cmp)
        themeBase = tomlFormat.generate "yazi-theme-base.toml" {
          mgr = {
            cwd = { fg = accent; };

            find_keyword  = { fg = "#CB9779"; italic = true; };
            find_position = { fg = "#AC8B7A"; bg = "reset"; italic = true; };

            marker_copied   = { fg = "#ACA097"; bg = "#ACA097"; };
            marker_cut      = { fg = "#978479"; bg = "#978479"; };
            marker_marked   = { fg = "#B7A699"; bg = "#B7A699"; };
            marker_selected = { fg = accent; bg = accent; };

            count_copied   = { fg = "#19120e"; bg = "#ACA097"; };
            count_cut      = { fg = "#19120e"; bg = "#978479"; };
            count_selected = { fg = "#19120e"; bg = accent; };

            border_symbol = "│";
            border_style  = { fg = "#000000"; };
          };

          # Hovered file (was mgr.hovered / preview_hovered)
          indicator = {
            parent  = { fg = "#19120e"; bg = accent; };
            current = { fg = "#19120e"; bg = accent; };
            preview = { underline = true; };
          };

          tabs = {
            active   = { fg = "#19120e"; bg = accent; };
            inactive = { fg = "#c5c3c2"; bg = "#444444"; };
          };

          mode = {
            normal_main = { fg = "#19120e"; bg = accent; bold = true; };
            normal_alt  = { fg = accent; bg = "#444444"; };
            select_main = { fg = "#19120e"; bg = "#ACA097"; bold = true; };
            select_alt  = { fg = "#ACA097"; bg = "#444444"; };
            unset_main  = { fg = "#19120e"; bg = "#B7A699"; bold = true; };
            unset_alt   = { fg = "#B7A699"; bg = "#444444"; };
          };

          status = {
            sep_left  = { open = ""; close = ""; };
            sep_right = { open = ""; close = ""; };

            progress_label  = { bold = true; };
            progress_normal = { fg = "#AF9789"; bg = "#19120e"; };
            progress_error  = { fg = "#978479"; bg = "#19120e"; };

            perm_type  = { fg = "#c5c3c2"; };
            perm_read  = { fg = "#CB9779"; };
            perm_write = { fg = "#978479"; };
            perm_exec  = { fg = "#ACA097"; };
            perm_sep   = { fg = "#6f615b"; };
          };

          which = {
            mask            = { bg = "#444444"; };
            cand            = { fg = "#c5c3c2"; };
            rest            = { fg = "#6f615b"; };
            desc            = { fg = accent; };
            separator       = "  ";
            separator_style = { fg = "#444444"; };
          };

          input   = { border = { fg = "#000000"; }; selected = { reversed = true; }; };
          pick    = { border = { fg = "#000000"; }; active = { fg = "#CB9779"; }; };
          confirm = { border = { fg = "#000000"; }; title = { fg = accent; }; };
          tasks   = { border = { fg = "#000000"; }; hovered = { underline = true; }; };
          cmp     = { border = { fg = "#000000"; }; active = { bg = "#444444"; }; };
          spot    = { border = { fg = "#000000"; }; };

          notify = {
            title_info  = { fg = "#ACA097"; };
            title_warn  = { fg = "#CB9779"; };
            title_error = { fg = "#6f615b"; };
          };
        };

        # Yazi's full built-in icon set (glyphs kept), recolored to the wallpaper accent
        theme = pkgs.runCommand "yazi-theme.toml" { } ''
          cat ${themeBase} > "$out"
          echo >> "$out"
          sed -n '/^\[icon\]/,$p' ${pkgs.yazi-unwrapped.srcs.code_src}/yazi-config/preset/theme-dark.toml \
            | sed -E 's/fg = "#[0-9a-fA-F]+"/fg = "${accent}"/g' >> "$out"
        '';
      in
      {
        programs.yazi = {
          enable = true;
          # Keep the legacy `yy` shell wrapper; silences the stateVersion < 26.05 default-change warning
          shellWrapperName = "yy";

          settings = {
            open.rules = [
              { mime = "text/*"; use = "edit"; }
              { mime = "image/*"; use = "image"; }
              { mime = "video/*"; use = "video"; }
              { mime = "{audio,application}/*"; use = "video"; }
            ];
            opener = {
              edit  = [{ run = ''nvim "$@"''; desc = "Edit in NVIM"; block = true; }];
              image = [{ run = ''loupe "$@"''; block = true; orphan = true; }];
              video = [{ run = ''mpv "$@"''; desc = "Open with mpv"; }];
            };
          };
        };

        # programs.yazi.theme can't merge with the recolored icon section, so write the file directly
        xdg.configFile."yazi/theme.toml".source = theme;

        home.packages = [ pkgs.mpv pkgs.loupe ];
      })
  ];
}
