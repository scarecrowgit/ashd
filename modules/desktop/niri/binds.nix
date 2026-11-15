# The keybind table as data. One list drives three outputs:
#   kdl        -> the `binds { }` block and the `recent-windows { binds { } }` block of
#                 /etc/niri/config.kdl
#   json       -> /etc/ashd/keybinds.json (the shell's cheatsheet)
#   duplicates -> asserted empty in niri.nix (no key combination is bound twice, across
#                 both blocks)
# docs/KEYBINDS.md is written by hand from the same table.
#
# Mod is Super on a TTY session (Alt when niri runs nested).
# niri's recent-windows switcher binds both Alt and Super + Tab / grave by default; the
# `mru` entries below replace those defaults with the Super ones only, so Alt+Tab reaches
# the applications. Pointer drags are built into niri and the fork (Mod+LMB move, Mod+RMB
# resize, Mod+MMB / Mod+LMB on the empty plane pan the canvas).
#
# Group order below is the order of the cheatsheet sections. The cheatsheet shows
# groups whose name contains "canvas" only while the focused workspace is a canvas.
{ lib, cfg }:

let
  q = s: ''"${s}"'';
  shellOn = cfg.shell.enable;
  apps = cfg.apps;

  # Action constructors; each yields the KDL action text.
  spawn = args: "spawn ${lib.concatMapStringsSep " " q args}";
  ashd = args: spawn ([ "ashd" ] ++ args);
  # Run `shell` when the ashd shell is enabled, `fallback` otherwise (null = omit bind).
  ifShell = shell: fallback: if shellOn then shell else fallback;
  # The fork's `if-canvas { <canvas action>; <strip action>; }`: first child on a canvas
  # workspace, second (optional) on a strip workspace.
  ifCanvas = canvas: strip:
    "if-canvas { ${lib.concatStringsSep "; " ([ canvas ] ++ lib.optional (strip != null) strip)}; }";

  b = group: keys: title: action: { inherit group keys title action; };
  # Same, allowed while the session is locked.
  bl = group: keys: title: action: (b group keys title action) // { allowWhenLocked = true; };
  # Same, with a repeat cooldown (wheel binds fire once per notch otherwise).
  bc = ms: group: keys: title: action: (b group keys title action) // { cooldownMs = ms; };
  # A recent-windows switcher bind (`next-window` / `previous-window`), rendered into
  # `recent-windows { binds { } }`; the switcher stays open while a modifier is held.
  bm = group: keys: title: action: (b group keys title action) // { mru = true; };

  workspaceNums = lib.genList (i: i + 1) 9;
  # `focus-workspace N` counts only visible workspaces in the fork, so the hidden
  # scratchpad workspace (workspaces.nix) does not shift the numbers. With the fork's
  # `create-workspaces-by-index` (layout.nix) N is created if it does not exist yet.

  entries = lib.filter (e: e.action != null) (
    [
      # ---- apps ----
      (b "Apps" "Mod+Space" "Launcher" (ifShell (ashd [ "launcher" ]) null))
      (b "Apps" "Mod+Ctrl+Space" "Emoji picker" (ifShell (ashd [ "launcher" "--mode" "emoji" ]) null))
      (b "Apps" "Mod+Return" "Terminal" (spawn [ apps.terminal ]))
      # `ashd fm` starts fm in the current theme (fm has no generated theme file).
      (b "Apps" "Mod+E" "File manager"
        (if shellOn && apps.fileManager == "fm" then ashd [ "fm" ] else spawn [ apps.fileManager ]))
      (b "Apps" "Mod+B" "Browser" (spawn [ apps.browser ]))

      # ---- windows ----
      (b "Windows" "Mod+Q" "Quit app" (ifShell (ashd [ "app" "quit" ]) "close-window"))
      (b "Windows" "Mod+W" "Close window" "close-window")
      (b "Windows" "Mod+Up" "Focus window up" "focus-window-up")
      (b "Windows" "Mod+Down" "Focus window down" "focus-window-down")
      (b "Windows" "Mod+Shift+Up" "Move window up" "move-window-up")
      (b "Windows" "Mod+Shift+Down" "Move window down" "move-window-down")
      (b "Windows" "Mod+Ctrl+F" "Fullscreen" "fullscreen-window")
      (b "Windows" "Mod+Escape" "Overview" "toggle-overview")
      # Recent windows: a quick tap switches to the previous window, holding Super walks
      # the list (the switcher shows after 150 ms). `scope="all"` on purpose: without it
      # niri reopens the switcher in the scope last picked inside it (W workspace, O output,
      # S cycle; easy to hit with Super still held, e.g. Super+W), and on a workspace with
      # one window a quick Super+Tab then does nothing.
      (bm "Windows" "Mod+Tab" "Recent windows" ''next-window scope="all"'')
      (bm "Windows" "Mod+Shift+Tab" "Recent windows (back)" ''previous-window scope="all"'')
      (bm "Windows" "Mod+grave" "Recent windows of this app" ''next-window scope="all" filter="app-id"'')
      (bm "Windows" "Mod+Shift+grave" "Recent windows of this app (back)" ''previous-window scope="all" filter="app-id"'')

      # ---- columns (strip mode; on a canvas they act on the floating window or do nothing) ----
      (b "Columns" "Mod+Left" "Focus column left" "focus-column-left")
      (b "Columns" "Mod+Right" "Focus column right" "focus-column-right")
      (b "Columns" "Mod+Shift+Left" "Move column left" "move-column-left")
      (b "Columns" "Mod+Shift+Right" "Move column right" "move-column-right")
      (b "Columns" "Mod+R" "Cycle column width" "switch-preset-column-width")
      (b "Columns" "Mod+F" "Maximize column" "maximize-column")
      (b "Columns" "Mod+BracketLeft" "Consume or expel left" "consume-or-expel-window-left")
      (b "Columns" "Mod+BracketRight" "Consume or expel right" "consume-or-expel-window-right")
      (b "Columns" "Mod+C" "Center column" "center-column")
      # Wheel with Shift moves along the strip; on a canvas it pans.
      (bc 150 "Columns" "Mod+Shift+WheelScrollUp" "Scroll left (wheel)"
        (ifCanvas "canvas-pan-left" "focus-column-left"))
      (bc 150 "Columns" "Mod+Shift+WheelScrollDown" "Scroll right (wheel)"
        (ifCanvas "canvas-pan-right" "focus-column-right"))

      # ---- workspaces ----
      (b "Workspaces" "Mod+Ctrl+Up" "Workspace up" "focus-workspace-up")
      (b "Workspaces" "Mod+Ctrl+Down" "Workspace down" "focus-workspace-down")
      (b "Workspaces" "Mod+Ctrl+Shift+Up" "Move column to workspace up" "move-column-to-workspace-up")
      (b "Workspaces" "Mod+Ctrl+Shift+Down" "Move column to workspace down" "move-column-to-workspace-down")
    ]
    ++ map (n: b "Workspaces" "Mod+${toString n}" "Workspace ${toString n}" "focus-workspace ${toString n}") workspaceNums
    ++ map (n: b "Workspaces" "Mod+Ctrl+${toString n}" "Move column to workspace ${toString n}" "move-column-to-workspace ${toString n}") workspaceNums
    ++ [
      # ---- canvas (the fork's infinite canvas; most keys do something sensible in the strip too) ----
      (b "Canvas" "Mod+G" "Toggle strip / canvas" "toggle-workspace-mode")
      (b "Canvas" "Mod+Ctrl+C" "Toggle strip / canvas" "toggle-workspace-mode")
      (b "Canvas" "Mod+Alt+Left" "Pan left" (ifCanvas "canvas-pan-left" null))
      (b "Canvas" "Mod+Alt+Right" "Pan right" (ifCanvas "canvas-pan-right" null))
      (b "Canvas" "Mod+Alt+Up" "Pan up" (ifCanvas "canvas-pan-up" null))
      (b "Canvas" "Mod+Alt+Down" "Pan down" (ifCanvas "canvas-pan-down" null))
      (b "Canvas" "Mod+Equal" "Zoom in (strip: wider column)" (ifCanvas "canvas-zoom-in" ''set-column-width "+10%"''))
      (b "Canvas" "Mod+Minus" "Zoom out (strip: narrower column)" (ifCanvas "canvas-zoom-out" ''set-column-width "-10%"''))
      (b "Canvas" "Mod+0" "Fit all windows (strip: center column)" (ifCanvas "canvas-fit" "center-column"))
      (b "Canvas" "Mod+Ctrl+0" "Zoom 100%" "canvas-set-zoom 1.0")
      (b "Canvas" "Mod+Home" "Pan to origin" "canvas-home")
      (b "Canvas" "Mod+P" "Pin window to the screen" "toggle-window-pinned")
      # Mod+wheel: switch workspace in the strip. Over a canvas the fork zooms natively and
      # continuously at the pointer (`wheel-zoom-factor`, no cooldown) before these binds are
      # looked at; the canvas branch only fires where that does not apply (e.g. pointer over the
      # bar) and stays here so the shell's HUD/cheatsheet find the zoom keys.
      (bc 150 "Canvas" "Mod+WheelScrollUp" "Zoom in at pointer (strip: workspace up)"
        (ifCanvas "canvas-zoom-in at-cursor=true" "focus-workspace-up"))
      (bc 150 "Canvas" "Mod+WheelScrollDown" "Zoom out at pointer (strip: workspace down)"
        (ifCanvas "canvas-zoom-out at-cursor=true" "focus-workspace-down"))

      # ---- scratchpad (the hidden workspace, ashd.scratchpad.name) ----
      (b "Scratchpad" "Mod+H" "Hide to scratchpad" (ifShell (ashd [ "scratch" "hide" ]) null))
      (b "Scratchpad" "Mod+M" "Hide to scratchpad" (ifShell (ashd [ "scratch" "hide" ]) null))
      (b "Scratchpad" "Mod+Shift+H" "Show last hidden window" (ifShell (ashd [ "scratch" "show" ]) null))
      (b "Scratchpad" "Mod+S" "Scratchpad panel" (ifShell (ashd [ "scratch" "toggle" ]) null))

      # ---- shell panels ----
      (b "Shell" "Mod+N" "Notifications" (ifShell (ashd [ "notifications" ]) null))
      (b "Shell" "Mod+Shift+V" "Clipboard history" (ifShell (ashd [ "clipboard" ]) null))
      (b "Shell" "Mod+Comma" "Quick settings" (ifShell (ashd [ "quick-settings" ]) null))
      (b "Shell" "Mod+Shift+C" "Calendar" (ifShell (ashd [ "calendar" ]) null))
      (b "Shell" "Mod+Shift+M" "Media player" (ifShell (ashd [ "media" ]) null))
      (b "Shell" "Mod+Shift+T" "Theme switcher" (ifShell (ashd [ "theme" "pick" ]) null))
      (b "Shell" "Mod+Slash" "Keybind cheatsheet" (ifShell (ashd [ "cheatsheet" ]) "show-hotkey-overlay"))

      # ---- media and brightness keys ----
      (bl "Media" "XF86AudioRaiseVolume" "Volume up"
        (ifShell (ashd [ "osd" "volume" "+5" ]) (spawn [ "wpctl" "set-volume" "-l" "1" "@DEFAULT_AUDIO_SINK@" "5%+" ])))
      (bl "Media" "XF86AudioLowerVolume" "Volume down"
        (ifShell (ashd [ "osd" "volume" "-5" ]) (spawn [ "wpctl" "set-volume" "@DEFAULT_AUDIO_SINK@" "5%-" ])))
      (bl "Media" "XF86AudioMute" "Mute"
        (ifShell (ashd [ "osd" "volume" "mute" ]) (spawn [ "wpctl" "set-mute" "@DEFAULT_AUDIO_SINK@" "toggle" ])))
      (bl "Media" "XF86AudioMicMute" "Mute microphone"
        (ifShell (ashd [ "osd" "mic-mute" ]) (spawn [ "wpctl" "set-mute" "@DEFAULT_AUDIO_SOURCE@" "toggle" ])))
      (bl "Media" "XF86MonBrightnessUp" "Brightness up" (ifShell (ashd [ "osd" "brightness" "+5" ]) null))
      (bl "Media" "XF86MonBrightnessDown" "Brightness down" (ifShell (ashd [ "osd" "brightness" "-5" ]) null))
      (bl "Media" "XF86AudioPlay" "Play / pause" (ifShell (ashd [ "media" "play-pause" ]) null))
      (bl "Media" "XF86AudioNext" "Next track" (ifShell (ashd [ "media" "next" ]) null))
      (bl "Media" "XF86AudioPrev" "Previous track" (ifShell (ashd [ "media" "previous" ]) null))
      (bl "Media" "XF86AudioStop" "Stop playback" (ifShell (ashd [ "media" "stop" ]) null))

      # ---- screenshots and recording (ashd screenshot overlay; niri built-ins without the shell) ----
      (b "Screenshots" "Mod+Shift+3" "Screenshot screen" (ifShell (ashd [ "screenshot" "--output" ]) "screenshot-screen"))
      (b "Screenshots" "Mod+Shift+4" "Screenshot region" (ifShell (ashd [ "screenshot" "--region" ]) "screenshot"))
      (b "Screenshots" "Mod+Shift+5" "Screenshot toolbar" (ifShell (ashd [ "screenshot" ]) "screenshot-window"))
      (b "Screenshots" "Mod+Shift+7" "Screenshot window" (ifShell (ashd [ "screenshot" "--window" ]) null))
      (b "Screenshots" "Mod+Shift+6" "Start / stop recording" (ifShell (ashd [ "record" "toggle" ]) null))
      (b "Screenshots" "Print" "Screenshot toolbar" (ifShell (ashd [ "screenshot" ]) "screenshot"))

      # ---- session ----
      (b "Session" "Mod+Ctrl+Q" "Lock screen" (ifShell (ashd [ "lock" ]) null))
      (b "Session" "Mod+Alt+Escape" "Session menu" (ifShell (ashd [ "session" ]) null))
    ]
  );

  renderBind = e:
    "${e.keys}"
    + lib.optionalString (e ? cooldownMs) " cooldown-ms=${toString e.cooldownMs}"
    + " hotkey-overlay-title=${q e.title}"
    + lib.optionalString (e.allowWhenLocked or false) " allow-when-locked=true"
    + " { ${e.action}; }";

  isMru = e: e.mru or false;
  renderBlock = indent: es: lib.concatMapStringsSep "\n" (e: indent + renderBind e) es;

  display = keys: lib.replaceStrings [ "Mod+" ] [ "Super+" ] keys;

  # niri ignores modifier order and key-name case: compare on a canonical spelling.
  canonical = keys:
    let parts = lib.splitString "+" (lib.toLower keys);
    in lib.concatStringsSep "+" (lib.sort lib.lessThan (lib.init parts) ++ [ (lib.last parts) ]);
  canonicalKeys = map (e: canonical e.keys) entries;
  duplicates = lib.unique (lib.filter (k: lib.count (x: x == k) canonicalKeys > 1) canonicalKeys);
in
{
  inherit entries duplicates;

  kdl = ''
    binds {
    ${renderBlock "    " (lib.filter (e: !isMru e) entries)}
    }

    // The switcher binds replace niri's defaults (which also take Alt+Tab).
    recent-windows {
        binds {
    ${renderBlock "        " (lib.filter isMru entries)}
        }
    }
  '';

  json = builtins.toJSON (map
    (e: {
      keys = display e.keys;
      niri = e.keys;
      inherit (e) title group action;
    })
    entries);
}
