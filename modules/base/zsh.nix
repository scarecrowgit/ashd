{ inputs, pkgs, config, ... }:

let
  self = inputs.self;
in
{
  programs.zsh.enable = true;

  home-manager.sharedModules = [
    (
      { config, ... }:
      {
        programs.zsh = {
          enable = true;
          autosuggestion.enable = false;
          syntaxHighlighting.enable = false;
          enableCompletion = false;
          defaultKeymap = "viins";
          history.size = 100000;
          history.path = "${config.xdg.dataHome}/zsh/history";
          dotDir = "${config.xdg.configHome}/zsh";
          plugins = [
            {
              name = "nix-zsh-completions";
              src = pkgs.nix-zsh-completions;
            }
          ];
          initContent = ''
            fpath=(${pkgs.nix-zsh-completions}/share/zsh/site-functions $fpath)

            source ${pkgs.zsh-defer}/share/zsh-defer/zsh-defer.plugin.zsh

            # sourced synchronously (not zsh-defer'd) so the up/down arrow
            # bindings are active immediately, before the first keypress
            source ${pkgs.zsh-history-substring-search}/share/zsh-history-substring-search/zsh-history-substring-search.zsh
            # bind both normal-mode (^[[A) and application-cursor-mode (^[OA)
            # sequences: zle's smkx switches kitty (and others) between the two
            bindkey "^[[A" history-substring-search-up
            bindkey "^[[B" history-substring-search-down
            bindkey "^[OA" history-substring-search-up
            bindkey "^[OB" history-substring-search-down

            zsh-defer -c 'autoload -Uz compinit && compinit -C'
            zsh-defer -c 'source ${pkgs.zsh-syntax-highlighting}/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh'
            zsh-defer -c 'source ${pkgs.zsh-autosuggestions}/share/zsh-autosuggestions/zsh-autosuggestions.zsh; ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE="fg=8"'
            zsh-defer -c 'source ${pkgs.zsh-fzf-tab}/share/fzf-tab/fzf-tab.plugin.zsh'
            zsh-defer -c 'eval "$(direnv hook zsh)"' 2>/dev/null
            zsh-defer -c 'eval "$(zoxide init zsh)"' 2>/dev/null

            sudo-command-line() {
              [[ -z $BUFFER ]] && zle up-history
              if [[ $BUFFER == sudo\ * ]]; then
                LBUFFER="''${LBUFFER#sudo }"
              else
                LBUFFER="sudo $LBUFFER"
              fi
            }
            zle -N sudo-command-line
            bindkey '\e\e' sudo-command-line

            # talosctl guard: machine config changes only with --mode=try (auto-rollback)
            # or --dry-run. 2026-10-01 a --mode=no-reboot network patch took
            # talos-worker02 fully offline and it had to be reinstalled.
            talosctl() {
              local sub="" a
              for a in "$@"; do
                case "$a" in
                  patch|apply-config|apply|edit) sub=$a; break ;;
                esac
              done
              if [[ -n $sub && " $* " != *" --mode=try "* && " $* " != *" --mode try "* \
                    && " $* " != *" -m try "* && " $* " != *" -m=try "* && " $* " != *" --dry-run "* ]]; then
                print -u2 "talosctl $sub without --mode=try: no auto-rollback if the node loses network (worker02, 2026-10-01)."
                print -u2 "Prefer: talosctl ... $sub ... --mode=try --timeout 2m"
                read -q "?Run anyway? [y/N] " || { print -u2; return 1 }
                print -u2
              fi
              command talosctl "$@"
            }

            # vi-backward-delete-char/kill-word stop at line starts and at the
            # insert-mode entry point; use the unrestricted widgets instead
            bindkey -M viins '^?' backward-delete-char
            bindkey -M viins '^H' backward-delete-char
            bindkey -M viins '^W' backward-kill-word
            # 0.1s: Esc enters normal mode quickly, but a fast Esc Esc still hits sudo-command-line
            KEYTIMEOUT=10

            autoload -Uz edit-command-line
            zle -N edit-command-line
            bindkey -M vicmd v edit-command-line

            bindkey '^a' beginning-of-line
            bindkey '^e' end-of-line
            bindkey '^[[1;5D' beginning-of-line
            bindkey '^[[1;5C' end-of-line
            forward-3-chars() { zle forward-char; zle forward-char; zle forward-char }
            backward-3-chars() { zle backward-char; zle backward-char; zle backward-char }
            zle -N forward-3-chars
            zle -N backward-3-chars
            bindkey '^[[1;2D' backward-3-chars
            bindkey '^[[1;2C' forward-3-chars

            unsetopt menu_complete
            unsetopt flowcontrol

            setopt prompt_subst
            setopt always_to_end
            setopt append_history
            setopt auto_menu
            setopt complete_in_word
            setopt extended_history
            setopt hist_expire_dups_first
            setopt hist_ignore_dups
            setopt hist_ignore_space
            setopt hist_verify
            setopt inc_append_history
            setopt share_history

            # ---- Syntax highlight styles ----
            # ANSI palette indices, not hex: kitty's palette comes from the ashd theme
            # (generated/kitty.conf), so these follow theme changes live.
            typeset -A ZSH_HIGHLIGHT_STYLES
            ZSH_HIGHLIGHT_STYLES[comment]='fg=8,italic'
            ZSH_HIGHLIGHT_STYLES[alias]='fg=4'
            ZSH_HIGHLIGHT_STYLES[suffix-alias]='fg=4'
            ZSH_HIGHLIGHT_STYLES[global-alias]='fg=4'
            ZSH_HIGHLIGHT_STYLES[function]='fg=5'
            ZSH_HIGHLIGHT_STYLES[command]='fg=4'
            ZSH_HIGHLIGHT_STYLES[precommand]='fg=4,italic'
            ZSH_HIGHLIGHT_STYLES[autodirectory]='fg=3,italic'
            ZSH_HIGHLIGHT_STYLES[single-hyphen-option]='fg=3'
            ZSH_HIGHLIGHT_STYLES[double-hyphen-option]='fg=3'
            ZSH_HIGHLIGHT_STYLES[back-quoted-argument]='fg=5'
            ZSH_HIGHLIGHT_STYLES[builtin]='fg=4'
            ZSH_HIGHLIGHT_STYLES[reserved-word]='fg=5'
            ZSH_HIGHLIGHT_STYLES[commandseparator]='fg=5'
            ZSH_HIGHLIGHT_STYLES[command-substitution-delimiter]='fg=6'
            ZSH_HIGHLIGHT_STYLES[back-quoted-argument-delimiter]='fg=5'
            ZSH_HIGHLIGHT_STYLES[back-double-quoted-argument]='fg=2'
            ZSH_HIGHLIGHT_STYLES[single-quoted-argument]='fg=2'
            ZSH_HIGHLIGHT_STYLES[double-quoted-argument]='fg=2'
            ZSH_HIGHLIGHT_STYLES[dollar-quoted-argument]='fg=6'
            ZSH_HIGHLIGHT_STYLES[dollar-double-quoted-argument]='fg=6'
            ZSH_HIGHLIGHT_STYLES[assign]='fg=6'
            ZSH_HIGHLIGHT_STYLES[unknown-token]='fg=1'
            ZSH_HIGHLIGHT_STYLES[path]='underline'
            ZSH_HIGHLIGHT_STYLES[path_pathseparator]='fg=4,underline'
            ZSH_HIGHLIGHT_STYLES[globbing]='fg=3'
            ZSH_HIGHLIGHT_STYLES[history-expansion]='fg=5'
            ZSH_HIGHLIGHT_STYLES[back-quoted-argument-unclosed]='fg=1'
            ZSH_HIGHLIGHT_STYLES[single-quoted-argument-unclosed]='fg=1'
            ZSH_HIGHLIGHT_STYLES[double-quoted-argument-unclosed]='fg=1'
            ZSH_HIGHLIGHT_STYLES[dollar-quoted-argument-unclosed]='fg=1'
            ZSH_HIGHLIGHT_STYLES[redirection]='fg=3'
            ZSH_HIGHLIGHT_STYLES[arg0]='fg=4'
            ZSH_HIGHLIGHT_STYLES[default]='none'

            zsh-defer -c '
              zstyle ":completion:*" menu select
              zstyle ":completion:*" list-colors "''${(s.:.)LS_COLORS}"
              zstyle ":completion:*" verbose yes
              zstyle ":completion:*:descriptions" format "%F{4}-- %d --%f"
              zstyle ":completion:*:messages" format "%F{5}-- %d --%f"
              zstyle ":completion:*:warnings" format "%F{1}-- no matches found --%f"
              zstyle ":completion:*" group-name ""
              zstyle ":completion:*:*:-command-:*:*" group-order aliases builtins functions commands
              zstyle ":completion:*" matcher-list "m:{a-zA-Z}={A-Za-z}" "r:|[._-]=* r:|=*" "l:|=* r:|=*"
              zstyle ":completion:*" extra-verbose yes
              zstyle ":completion:*" use-cache on
              zstyle ":completion:*" cache-path "$XDG_CACHE_HOME/zsh/.zcompcache"
              zstyle ":completion:*" file-list all
              zstyle ":completion:*:options" description yes
              zstyle ":completion:*:options" auto-description "%d"
            '
          '';
          envExtra = ''
            export NPM_CONFIG_PREFIX="$HOME/.npm-global"
            export PATH="$HOME/.npm-global/bin:$PATH"

            export EDITOR="nvim"
            export VISUAL="nvim"
            export GNUPGHOME="$HOME/.private/gnupg"

            export XMONAD_CONFIG_DIR="''${XDG_CONFIG_HOME:-$HOME/.config}/xmonad"
            export XMONAD_DATA_DIR="''${XDG_DATA_HOME:-$HOME/.local/share}/xmonad"
            export XMONAD_CACHE_DIR="''${XDG_CACHE_HOME:-$HOME/.cache}/xmonad"

            export FZF_DEFAULT_OPTS=" \
            --color=bg+:8,bg:-1,spinner:6,hl:4 \
            --color=fg:-1,header:1,info:5,pointer:5 \
            --color=marker:2,fg+:15,prompt:4,hl+:12"
          '';
          shellGlobalAliases = {
            UUID = "$(uuidgen | tr -d \\n)";
            G = "| grep";
          };
          shellAliases = {
            ".." = "cd ..";
            "..." = "cd ../..";
            lf = ''
                {
                  tmp="$(mktemp)"
                  command lf -last-dir-path="$tmp" "$@"
                  if [ -f "$tmp" ]; then
                      dir="$(cat "$tmp")"
                      rm -f "$tmp"
                      if [ -d "$dir" ]; then
                          if [ "$dir" != "$(pwd)" ]; then
                              cd "$dir"
                          fi
                      fi
                  fi
                }
            '';
            cls = "clear";
            tml = "tmux list-sessions";
            tma = "tmux attach";
            l = "${pkgs.eza}/bin/eza -lh  --icons=auto";
            ls = "${pkgs.eza}/bin/eza -1   --icons=auto";
            ll = "${pkgs.eza}/bin/eza -lha --icons=auto --sort=name --group-directories-first";
            la = "${pkgs.eza}/bin/eza -la  --icons=auto";
            ld = "${pkgs.eza}/bin/eza -lhD --icons=auto";
            tree = "${pkgs.eza}/bin/eza --icons=auto --tree";
            v = "nvim";
            vim = "nvim";
            nv = "nvim";
            vc = "code --disable-gpu";
            cp = "cp -iv";
            mv = "mv -iv";
            rm = "rm -vI";
            bc = "bc -ql";
            mkd = "mkdir -pv";
            grep = "grep --color=always";

            wttr = "curl wttr.in/Batumi";

            g   = "git";
            gs  = "git status";
            ga  = "git add";
            gc  = "git commit";
            gp  = "git push";
            gl  = "git log";
            gd  = "git diff";
            gco = "git checkout";
            gb  = "git branch";

            rebuild = "sudo nixos-rebuild switch --flake ~/nix-new#Ivant-main";
            update  = "nix flake update";
            clean   = "sudo nix-collect-garbage -d";
            list-gens = "nixos-rebuild list-generations";
            update-input = "nix flake update $@";
          };
        };
      }
    )
  ];
}
