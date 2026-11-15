# Keybinds

Source of truth: `modules/desktop/niri/binds.nix` (Nix data). It is rendered to the
`binds { }` block of `/etc/niri/config.kdl` and, when `ashd.shell.enable` is set, to
`/etc/ashd/keybinds.json` for the shell's cheatsheet (`Super+/`). This file mirrors that
table; regenerate it from `/etc/ashd/keybinds.json` (`nix eval --raw
'.#nixosConfigurations.Ivant-main.config.environment.etc."ashd/keybinds.json".text'`) when
the Nix data changes. A Nix assertion fails the build if a key combination is bound twice
(modifier order and key-name case do not matter).

`Super` is niri's `Mod` (it is `Alt` when niri runs nested inside another compositor).
Entries marked **shell** are only bound when `ashd.shell.enable = true`; where a fallback
exists it is bound otherwise. The recent-windows switcher keys (Super+Tab, Super+grave)
are niri's switcher, generated into `recent-windows { binds { } }`; they replace niri's
defaults, which also took Alt+Tab, so Alt+Tab reaches the applications. While the
switcher is open (Super held), W, O and S narrow it to the workspace / output or cycle the
scope; every Super+Tab starts again with all windows (`scope="all"`; niri would otherwise
keep the last scope, and a quick Super+Tab on a workspace with one window did nothing).

Rows with `if-canvas { A; B; }` do `A` when the focused workspace is a canvas and `B` when
it is a strip (`B` missing: nothing). Super+G (or Super+Ctrl+C) switches the focused
workspace between the two; the theme decides the mode of new workspaces (Ink and Moss start as canvas).
The cheatsheet shows the **Canvas** group only while the focused workspace is a canvas.

The hidden scratchpad workspace (`ashd.scratchpad`, default name `scratch`) does not count
in workspace numbers: the fork's `focus-workspace N` and `move-column-to-workspace N` count
visible workspaces only, so Super+1..9 are plain `focus-workspace 1..9` on every monitor.
Workspaces are created by number (the fork's `create-workspaces-by-index`, set in
`modules/desktop/niri/layout.nix`): Super+5 with two workspaces creates workspaces 3, 4
and 5 and goes to 5, and Super+Ctrl+5 moves the column there. An empty workspace stays as
long as a higher-numbered one has windows or is focused, so the numbers never shift; only
the empty workspaces after the last used one are removed.

Fullscreen (the fork's `fullscreen-workspaces` and `edge-reveal`, `modules/desktop/niri/layout.nix`):
a fullscreen window lives on its own workspace right after the one it came from; it has no
number (Super+1..9 skip it) and Super+Ctrl+Up/Down reach it. New windows opened from it open
on that origin workspace. Over a fullscreen window, rest the pointer against the left edge
(rail) or the bottom edge (bar and dock) to slide them in; they slide out when the pointer
leaves them. Not while a game holds the pointer, a button is held or a menu grab is open.

Pointer (built in, not in the table): Super+left-drag moves a window, Super+right-drag
resizes it, Super+middle-drag pans a canvas, Super+left-drag on the empty plane pans it,
a title-strip drag moves the window, a double click on a strip maximizes the column (strip)
or zooms to 100% / fits all windows (canvas), a touchpad three-finger swipe pans the canvas,
and a pinch zooms it.

## Apps

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Space | `ashd launcher` | **shell** |
| Super+Ctrl+Space | `ashd launcher --mode emoji` | **shell** |
| Super+Return | terminal (`ashd.apps.terminal`, kitty) | |
| Super+E | file manager (`ashd.apps.fileManager`, fm) | |
| Super+B | browser (`ashd.apps.browser`, zen-twilight) | |

## Windows

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Q | `ashd app quit` | **shell**; fallback `close-window` |
| Super+W | `close-window` | |
| Super+Up | `focus-window-up` | |
| Super+Down | `focus-window-down` | |
| Super+Shift+Up | `move-window-up` | |
| Super+Shift+Down | `move-window-down` | |
| Super+Ctrl+F | `fullscreen-window` | the window gets a workspace of its own after the current one (no number); again: back to its place |
| Super+Escape | `toggle-overview` | |
| Super+Tab | `next-window scope="all"` (recent windows) | tap: previous window; hold Super and tap Tab: walk the list |
| Super+Shift+Tab | `previous-window scope="all"` (recent windows) | |
| Super+grave | `next-window scope="all" filter="app-id"` (recent windows) | windows of the focused app |
| Super+Shift+grave | `previous-window scope="all" filter="app-id"` (recent windows) | |

## Columns

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Left | `focus-column-left` | |
| Super+Right | `focus-column-right` | |
| Super+Shift+Left | `move-column-left` | |
| Super+Shift+Right | `move-column-right` | |
| Super+R | `switch-preset-column-width` | |
| Super+F | `maximize-column` | |
| Super+BracketLeft | `consume-or-expel-window-left` | |
| Super+BracketRight | `consume-or-expel-window-right` | |
| Super+C | `center-column` | |
| Super+Shift+WheelScrollUp | `if-canvas { canvas-pan-left; focus-column-left; }` | 150 ms cooldown |
| Super+Shift+WheelScrollDown | `if-canvas { canvas-pan-right; focus-column-right; }` | 150 ms cooldown |

## Workspaces

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Ctrl+Up | `focus-workspace-up` | |
| Super+Ctrl+Down | `focus-workspace-down` | |
| Super+Ctrl+Shift+Up | `move-column-to-workspace-up` | |
| Super+Ctrl+Shift+Down | `move-column-to-workspace-down` | |
| Super+1 ... Super+9 | `focus-workspace N` | N counts visible workspaces only (the hidden scratchpad is skipped); creates workspace N if needed |
| Super+Ctrl+1 ... Super+Ctrl+9 | `move-column-to-workspace N` | same numbering; creates workspace N if needed |

## Canvas

| Keys | Action | Notes |
| --- | --- | --- |
| Super+G | `toggle-workspace-mode` | strip / canvas |
| Super+Ctrl+C | `toggle-workspace-mode` | same |
| Super+Alt+Left | `if-canvas { canvas-pan-left; }` | |
| Super+Alt+Right | `if-canvas { canvas-pan-right; }` | |
| Super+Alt+Up | `if-canvas { canvas-pan-up; }` | |
| Super+Alt+Down | `if-canvas { canvas-pan-down; }` | pans by `pan-step` (120 px); does nothing in the strip |
| Super+Equal | `if-canvas { canvas-zoom-in; set-column-width "+10%"; }` | zoom steps 0.4 0.55 0.75 1.0; holding the key keeps zooming |
| Super+Minus | `if-canvas { canvas-zoom-out; set-column-width "-10%"; }` | |
| Super+0 | `if-canvas { canvas-fit; center-column; }` | |
| Super+Ctrl+0 | `canvas-set-zoom 1.0` | |
| Super+Home | `canvas-home` | |
| Super+P | `toggle-window-pinned` | |
| Super+WheelScrollUp | `if-canvas { canvas-zoom-in at-cursor=true; focus-workspace-up; }` | over a canvas: continuous zoom at the pointer, x1.2 per notch, no cooldown (built in); strip: 150 ms cooldown |
| Super+WheelScrollDown | `if-canvas { canvas-zoom-out at-cursor=true; focus-workspace-down; }` | same; Super+two-finger scroll on a touchpad zooms too |

## Scratchpad

| Keys | Action | Notes |
| --- | --- | --- |
| Super+H, Super+M | `ashd scratch hide` | **shell** |
| Super+Shift+H | `ashd scratch show` | **shell** |
| Super+S | `ashd scratch toggle` | **shell** |

## Shell

| Keys | Action | Notes |
| --- | --- | --- |
| Super+N | `ashd notifications` | **shell** |
| Super+Shift+V | `ashd clipboard` | **shell** |
| Super+Comma | `ashd quick-settings` | **shell** |
| Super+Shift+C | `ashd calendar` | **shell** |
| Super+Shift+M | `ashd media` | **shell** |
| Super+Shift+T | `ashd theme pick` | **shell** |
| Super+Slash | `ashd cheatsheet` | **shell**; fallback `show-hotkey-overlay` |

## Media

| Keys | Action | Notes |
| --- | --- | --- |
| XF86AudioRaiseVolume | `ashd osd volume +5` | **shell**; fallback `wpctl set-volume` |
| XF86AudioLowerVolume | `ashd osd volume -5` | **shell**; fallback `wpctl set-volume` |
| XF86AudioMute | `ashd osd volume mute` | **shell**; fallback `wpctl set-mute` |
| XF86AudioMicMute | `ashd osd mic-mute` | **shell**; fallback `wpctl set-mute` |
| XF86MonBrightnessUp | `ashd osd brightness +5` | **shell** |
| XF86MonBrightnessDown | `ashd osd brightness -5` | **shell** |
| XF86AudioPlay | `ashd media play-pause` | **shell** |
| XF86AudioNext | `ashd media next` | **shell** |
| XF86AudioPrev | `ashd media previous` | **shell** |
| XF86AudioStop | `ashd media stop` | **shell** |

## Screenshots

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Shift+3 | `ashd screenshot --output` | **shell**; fallback `screenshot-screen` |
| Super+Shift+4 | `ashd screenshot --region` | **shell**; fallback `screenshot` |
| Super+Shift+5, Print | `ashd screenshot` | **shell**; fallback `screenshot-window` |
| Super+Shift+7 | `ashd screenshot --window` | **shell** |
| Super+Shift+6 | `ashd record toggle` | **shell** |

## Session

| Keys | Action | Notes |
| --- | --- | --- |
| Super+Ctrl+Q | `ashd lock` | **shell** |
| Super+Alt+Escape | `ashd session` | **shell** |
