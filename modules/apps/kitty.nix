{ ... }:

{
  # Declarative kitty config system-wide without home-manager. Static Ash palette;
  # `ashd theme apply` can override it through the include at the end.
  # KITTY_CONFIG_DIRECTORY makes kitty always load /etc/kitty/kitty.conf and
  # skip the ~/.config search, so it overrides any stale leftover dotfiles.
  environment.sessionVariables.KITTY_CONFIG_DIRECTORY = "/etc/kitty";

  environment.etc."kitty/kitty.conf".text = ''
    # Managed by nix-new. Colours: Ash, then ~/.local/state/ashd/generated/kitty.conf.
    font_family      JetBrainsMono Nerd Font
    font_size        11.0

    # Shell integration is sourced and configured manually
    shell_integration no-rc

    background_opacity 0.95
    confirm_os_window_close 0
    copy_on_select yes
    enable_audio_bell no
    mouse_hide_wait 60
    scrollback_lines 10000
    strip_trailing_spaces smart
    macos_option_as_alt yes
    macos_quit_when_last_window_closed yes
    update_check_interval 0

    # ---- colours (Ash) ----
    background            #1B1B19
    foreground            #DAD7CF
    cursor                #E0A458
    cursor_text_color     #1B1B19
    selection_background  #2A2A27
    selection_foreground  #DAD7CF
    url_color             #E0A458
    active_border_color   #E0A458
    inactive_border_color #2A2A27

    color0  #161615
    color1  #E07A62
    color2  #9DBF86
    color3  #E6C25A
    color4  #8DB0D6
    color5  #B79AC4
    color6  #86BFB5
    color7  #C2BEB4
    color8  #6B6A64
    color9  #E8947E
    color10 #B3D09E
    color11 #EED27A
    color12 #A6C4E4
    color13 #C9B0D3
    color14 #A0D0C7
    color15 #DAD7CF

    # ---- tab bar ----
    tab_bar_style powerline
    tab_powerline_style round
    tab_title_template {index}
    active_tab_font_style   bold
    inactive_tab_font_style normal
    tab_bar_background      #161615
    active_tab_foreground   #1B1B19
    active_tab_background   #E0A458
    inactive_tab_foreground #96938B
    inactive_tab_background #2A2A27

    # ---- keymaps (preserved from previous config) ----
    map alt+0 goto_tab 10
    map alt+1 goto_tab 1
    map alt+2 goto_tab 2
    map alt+3 goto_tab 3
    map alt+4 goto_tab 4
    map alt+5 goto_tab 5
    map alt+6 goto_tab 6
    map alt+7 goto_tab 7
    map alt+8 goto_tab 8
    map alt+9 goto_tab 9
    map alt+w copy_and_clear_or_interrupt
    map ctrl+alt+n launch --cwd=current
    map ctrl+shift+left no_op
    map ctrl+shift+right no_op
    map ctrl+t launch --cwd=current --type=overlay tmux-sessionizer
    map ctrl+y paste_from_clipboard

    # Written by `ashd theme apply` (kitty only logs a missing include); the CLI
    # live-reloads kitty with SIGUSR1 afterwards.
    include ~/.local/state/ashd/generated/kitty.conf
  '';
}
