{ lib, pkgs, ... }:
let
  # Static Ash palette, same names and values as the `[palettes.ashd]` table that
  # `ashd theme apply` generates (ARCHITECTURE.md section 8: generated/starship-palette.toml).
  # The prompt format below only uses these names, so either table works.
  palette = {
    bg = "#111110";
    surface = "#161615";
    surface_2 = "#1B1B19";
    line = "#2A2A27";
    frame = "#2A2A27";
    fg = "#DAD7CF";
    fg_muted = "#96938B";
    accent = "#E0A458";
    on_accent = "#161615";
    ok = "#9DBF86";
    warn = "#E6C25A";
    err = "#E07A62";
    info = "#8DB0D6";
    syn_keyword = "#E0A458";
    syn_string = "#C2BEB4";
    syn_number = "#DAD7CF";
    syn_comment = "#8A877F";
  };

  settings = {
    add_newline = true;
    command_timeout = 500;
    palette = "ashd";
    format = ''
      [┌───────────────────────────────────────────────────>](bold accent)
      [│](bold accent)$directory$git_branch$git_status$python$kubernetes$rust$golang$docker_context$nix_shell
      [└─>](bold accent) '';
    character = {
      success_symbol = "[➜](bold accent)";
      error_symbol = "[✗](bold err)";
    };
    directory = {
      truncation_length = 3;
      truncate_to_repo = true;
      format = "[$path]($style)[$read_only]($read_only_style) ";
      style = "bold fg";
      read_only = " 🔒";
      truncation_symbol = "…/";
    };
    git_branch = {
      symbol = "🌱 ";
      format = "on [$symbol$branch]($style) ";
      style = "bold accent";
    };
    git_status = {
      format = ''([\[$all_status$ahead_behind\]]($style) )'';
      style = "bold warn";
      conflicted = "🏳";
      ahead = "⇡\${count}";
      behind = "⇣\${count}";
      diverged = "⇕⇡\${ahead_count}⇣\${behind_count}";
      untracked = "🤷";
      stashed = "📦";
      modified = "📝";
      staged = ''[++\($count\)](ok)'';
      renamed = "👅";
      deleted = "🗑";
    };
    python = {
      symbol = "🐍 ";
      format = ''via [$\{symbol}$\{pyenv_prefix}($\{version} )(\($virtualenv\) )]($style)'';
      style = "bold warn";
      pyenv_version_name = false;
    };
    nodejs.disabled = true;
    kubernetes = {
      symbol = "☸ ";
      format = ''via [$symbol$context( \($namespace\))]($style) '';
      style = "bold info";
      disabled = false;
    };
    rust = {
      symbol = "🦀 ";
      format = "via [$symbol($version )]($style)";
      style = "bold warn";
    };
    golang = {
      symbol = "🐹 ";
      format = "via [$symbol($version )]($style)";
      style = "bold info";
    };
    docker_context = {
      symbol = "🐳 ";
      format = "via [$symbol$context]($style) ";
      style = "bold info";
      only_with_files = true;
    };
    cmd_duration = {
      min_time = 500;
      format = "took [$duration]($style) ";
      style = "bold warn";
    };
    nix_shell = {
      symbol = "❄️ ";
      format = "[$symbol]($style)";
      style = "bold info";
    };
  };

  tomlFormat = pkgs.formats.toml { };
in
{
  # The prompt settings without the palette: `ashd theme apply` appends the generated
  # palette to this file and writes the result to generated/starship.toml, which the
  # zsh init below selects through STARSHIP_CONFIG when it exists.
  environment.etc."ashd/starship-base.toml".source = tomlFormat.generate "starship-base.toml" settings;

  home-manager.sharedModules = [
    (_: {
      programs.starship = {
        enable = true;
        # Static fallback: base settings plus the Ash palette.
        settings = settings // { palettes.ashd = palette; };
      };

      # Evaluated once per shell start, not a prompt hook: starship re-reads the file
      # on every prompt, so later `ashd theme apply` runs show up in running shells too.
      programs.zsh.initContent = lib.mkOrder 500 ''
        _ashd_starship="''${ASHD_STATE_DIR:-''${XDG_STATE_HOME:-$HOME/.local/state}/ashd}/generated/starship.toml"
        [[ -r $_ashd_starship ]] && export STARSHIP_CONFIG=$_ashd_starship
        unset _ashd_starship
      '';
    })
  ];
}
