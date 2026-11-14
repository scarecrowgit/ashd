# Layout of the session: the static (Ash) values of the fork's layout, title strips,
# focus ring and canvas.
#
# How this file and the generated theme fragment combine. niri.nix ends the config with
#
#     include optional=true "~/.local/state/ashd/generated/niri.kdl"
#
# (rewritten by `ashd theme apply`, ashd-tokens `niri_kdl`). niri parses an included file
# into the SAME config object, in order, at the point of the `include` node, and every
# section merges field by field with what came before: niri-config `Config::parse`
# hands the include the shared config; `Layout::merge_with` merges focus-ring, border,
# shadow, insert-hint, canvas (incl. grid / region keys) and title-bar per field and
# overwrites only the keys the part contains. So this block can stay complete and the
# fragment only restates the theme-dependent keys. Checked with the fork's `niri
# validate`: a second `layout` node inside ONE file is a "duplicate node" error, while
# the same node in an included file is accepted and merged, and a bad value in the
# included layout is reported as "failed to parse included config" (README.md).
# Things that do not merge as one would expect, and the generator accounts for them:
#   - `struts` merges per side across includes, but a side set in both parts takes the
#     later value (the fragment's). The fragment writes `struts { top }` for every theme
#     (30 px strip header, plus the theme gap under floating themes), so once it exists
#     it decides the top strut and `ashd.layout.topStrut` below is only the value used
#     before the first `ashd theme apply`.
#   - `off` on border / focus-ring is sticky unless `on` is written later: both blocks
#     say `on` explicitly here and in the fragment.
#   - `window-rule`s accumulate; for the same property the later rule wins, so the
#     fragment's corner-radius rule (included last) overrides the one below.
# Without the shell (no generated file) the Ash values below are the whole story.
{ lib, cfg }:

let
  # Ash tokens (ashd/tokens/tokens.json, ashd-tokens): the fallback until `ashd theme
  # apply` has written the fragment.
  ash = {
    bg = "#111110";
    surface = "#161615";
    surface2 = "#1B1B19";
    line = "#2A2A27";
    frame = "#2A2A27";
    fg = "#DAD7CF";
    muted = "#96938B";
    accent = "#E0A458";
    err = "#E07A62";
    mono = "JetBrains Mono";
  };
in

''
  prefer-no-csd

  layout {
      gaps 12
      center-focused-column "always"
      // Theme default; the generated fragment sets it per theme (Ink and Moss: canvas).
      default-workspace-mode "strip"
      background-color "${ash.bg}"
  ${lib.optionalString (cfg.layout.topStrut != 0) ''
      // ashd.layout.topStrut: space at the top of the working area for the strip header
      // (the theme fragment overrides it).
      struts { top ${toString cfg.layout.topStrut}; }
  ''}
      preset-column-widths {
          proportion 0.33333
          proportion 0.5
          proportion 0.66667
      }
      default-column-width { proportion 0.5; }

      // 1px window frame; on a canvas it turns accent (canvas-style "border" below).
      border {
          on
          width 1
          active-color "${ash.frame}"
          inactive-color "${ash.frame}"
          urgent-color "${ash.err}"
      }
      // 1px accent ring 4px outside the focused window (strip mode).
      focus-ring {
          on
          width 1
          gap 4
          active-color "${ash.accent}"
          inactive-color "${ash.frame}"
          urgent-color "${ash.err}"
          canvas-style "border"
      }
      shadow { off; }

      // Compositor-drawn title strip above every window.
      title-bar {
          height 28
          font "${ash.mono} 11"
          padding 12
          background-color "${ash.surface2}"
          text-color "${ash.fg}"
          muted-color "${ash.muted}"
          line-color "${ash.line}"
          line-width 1
      }

      // The infinite canvas (mode of a workspace: strip or canvas).
      canvas {
          default-zoom 0.62
          zoom-steps 0.4 0.5 0.62 0.75 0.9 1.0
          pan-step 120
          grid "lines"
          grid-spacing 80
          grid-color "${ash.surface}"
          grid-line-width 1
          background-color "${ash.bg}"
          region-color "${ash.line}"
          region-label-color "${ash.muted}"
          region-font "${ash.mono} 16"
      }
  }

  window-rule {
      geometry-corner-radius 0
      clip-to-geometry true
  }
''
