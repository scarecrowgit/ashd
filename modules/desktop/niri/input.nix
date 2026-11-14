# Input, outputs and cursor, all driven by the ashd.* options.
{ lib, cfg }:

let
  q = s: ''"${s}"'';
  inherit (cfg) input outputs cursor;

  renderOutput = name: o: ''
    output ${q name} {
        mode ${q o.mode}
    ${lib.optionalString (o.scale != null) "    scale ${toString o.scale}\n"}}
  '';
in
''
  input {
      keyboard {
          xkb {
              layout ${q input.xkb.layout}
              ${lib.optionalString (input.xkb.options != "") "options ${q input.xkb.options}"}
          }
      }
      mouse {
          accel-profile ${q input.mouse.accelProfile}
          ${lib.optionalString input.mouse.naturalScroll "natural-scroll"}
      }
      ${lib.optionalString input.focusFollowsMouse ''focus-follows-mouse max-scroll-amount="0%"''}
      // niri's power-key handler requests suspend, which is disabled on this host;
      // logind (HandlePowerKey=poweroff) takes the key instead.
      disable-power-key-handling
  }

  ${lib.concatStringsSep "\n" (lib.mapAttrsToList renderOutput outputs)}
  cursor {
      xcursor-theme ${q cursor.name}
      xcursor-size ${toString cursor.size}
  }
''
