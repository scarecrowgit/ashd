# Short and critically damped: nothing bounces.
{ ... }:

''
  animations {
      // Windows open scaling .95 -> 1 while fading in, and close the reverse (design
      // motion spec); the same on strip and canvas workspaces.
      window-open {
          duration-ms 150
          curve "ease-out-expo"
          custom-shader r"
              vec4 open_color(vec3 coords_geo, vec3 size_geo) {
                  float scale = 0.95 + 0.05 * niri_clamped_progress;
                  coords_geo = vec3((coords_geo.xy - vec2(0.5)) / scale + vec2(0.5), 1.0);
                  vec3 coords_tex = niri_geo_to_tex * coords_geo;
                  vec4 color = texture2D(niri_tex, coords_tex.st);
                  return color * niri_clamped_progress;
              }
          "
      }
      window-close {
          duration-ms 150
          curve "ease-out-quad"
          custom-shader r"
              vec4 close_color(vec3 coords_geo, vec3 size_geo) {
                  float scale = 1.0 - 0.05 * niri_clamped_progress;
                  coords_geo = vec3((coords_geo.xy - vec2(0.5)) / scale + vec2(0.5), 1.0);
                  vec3 coords_tex = niri_geo_to_tex * coords_geo;
                  vec4 color = texture2D(niri_tex, coords_tex.st);
                  return color * (1.0 - niri_clamped_progress);
              }
          "
      }
      horizontal-view-movement { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      window-movement { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      window-resize { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      workspace-switch { spring damping-ratio=1.0 stiffness=1000 epsilon=0.0001; }
      config-notification-open-close { spring damping-ratio=1.0 stiffness=1000 epsilon=0.001; }
      overview-open-close { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      // Compositor dialogs follow the same rules (design: anything that appears is a short
      // tween, exits a third faster, springs are critically damped) instead of niri's defaults.
      exit-confirmation-open-close { spring damping-ratio=1.0 stiffness=1000 epsilon=0.001; }
      screenshot-ui-open {
          duration-ms 150
          curve "ease-out-expo"
      }
      recent-windows-close {
          duration-ms 100
          curve "ease-out-quad"
      }
      // The fork's canvas: pan / fly-to and zoom (design motion tokens, ashd/tokens/tokens.json).
      canvas-pan { spring damping-ratio=1.0 stiffness=500 epsilon=0.0001; }
      // Keyboard zoom; a held key retargets it and keeps its speed.
      canvas-zoom { spring damping-ratio=1.0 stiffness=600 epsilon=0.0001; }
      // Mod+scroll zoom: every notch retargets it and keeps its speed; higher stiffness =
      // snappier (keep damping-ratio >= 1.0: no overshoot past the zoom limits).
      canvas-wheel-zoom { spring damping-ratio=1.0 stiffness=1200 epsilon=0.0001; }
      // Strip <-> canvas switch: one transition, windows travel while the zoom changes along.
      canvas-mode-switch {
          duration-ms 280
          curve "ease-out-cubic"
      }
      // Edge panels sliding over fullscreen windows (edge-reveal, layout.nix).
      edge-reveal { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
  }
''
