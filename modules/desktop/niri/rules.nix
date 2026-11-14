# Window rules, layer rules and compositor debug flags.
{ lib, cfg }:

let
  # Apps that draw their own header look wrong with a second strip from the compositor:
  # ashd.windowRules.noTitleBar lists their app-id regexes (empty by default).
  # `prefer-no-csd` (layout.nix) already makes most apps drop their own decorations.
  noTitleBar = lib.optionalString (cfg.windowRules.noTitleBar != [ ]) ''
    window-rule {
    ${lib.concatMapStringsSep "\n" (re: "    match app-id=r#\"${re}\"#") cfg.windowRules.noTitleBar}
        title-bar false
    }
  '';

  static = ''
    // Picture-in-Picture (Firefox, Zen) floats instead of tiling.
    window-rule {
        match app-id=r#"^(firefox|zen|zen-twilight|zen-beta)$"# title="^Picture-in-Picture$"
        open-floating true
    }

    // fm in file-chooser mode (xdg-desktop-portal-termfilechooser -> fm --pick),
    // app-id set by fm itself.
    window-rule {
        match app-id="^fm-picker$"
        open-floating true
    }

    // Portal / polkit / file dialogs.
    window-rule {
        match app-id=r#"^(xdg-desktop-portal(-gtk|-gnome)?|org\.freedesktop\.impl\.portal\.desktop\.[a-z]+)$"#
        match app-id=r#"^(polkit-gnome-authentication-agent-1|soteria)$"#
        match title=r#"^(Open Files?|Open Folder|Save File|Save As|Select (a )?File)$"#
        open-floating true
    }

    // No layer rules yet: the ashd shell's layer surfaces set their own namespaces and
    // will get rules here once they exist.

    debug {
        // Discord, Telegram and friends request focus with fresh xdg-activation tokens
        // whose serial is invalid; honour them so tray icons and notifications focus.
        honor-xdg-activation-with-invalid-serial
    }
  '';
in
noTitleBar + static
