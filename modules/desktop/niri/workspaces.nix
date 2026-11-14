# The scratchpad: exactly one named workspace with `hidden`, which the fork keeps out of
# navigation, rendering, the overview and the window switcher (ASHD-FORK.md "Hidden
# workspaces"). `ashd scratch hide|show|toggle` moves windows to and from it with the
# fork's hide-window / unhide-window actions; the shell finds it by its `is_hidden` flag,
# so the name only has to be unique.
#
# It keeps its real index internally (named workspaces are inserted at index 1 of the
# monitor that owns them), but the fork's index references (`focus-workspace N`, ...) count
# visible workspaces only, so binds.nix uses plain 1..9.
{ lib, cfg }:

lib.optionalString cfg.scratchpad.enable ''
  workspace "${cfg.scratchpad.name}" {
      hidden
  }
''
