{
  config,
  hostVars,
  inputs,
  lib,
  pkgs,
  ...
}:
{
  # Same wrapped Zen as home-manager builds (policies, prefs), also on the system PATH.
  # Claude Desktop narrows PATH for xdg-open to /run/current-system/sw/bin and friends,
  # so `Exec=zen-twilight` from the user profile is not found and links never open.
  environment.systemPackages = [
    config.home-manager.users.${hostVars.userName}.programs.zen-browser.finalPackage
  ];
  home-manager.sharedModules = [
    (_: {
      imports = [ inputs.zen-browser.homeModules.twilight ];

      programs.zen-browser = {
        enable = true;
        policies = import ./policies.nix { inherit lib; };
        languagePacks = [
          "en-GB"
          "en-US"
        ];
        profiles = {
          default = {
            id = 0; # 0 is the default profile; see also option "isDefault"
            name = "default"; # name as listed in about:profiles
            isDefault = true; # can be omitted; true if profile ID is 0
            settings = import ./settings.nix;
            bookmarks = import ./bookmarks.nix;
            search = import ./search.nix { inherit pkgs; };
            userChrome = builtins.readFile ./userChrome.css;
            userContent = builtins.readFile ./userContent.css;
            mods = [
              "f7c71d9a-bce2-420f-ae44-a64bd92975ab" # Better Unloaded Tabs
            ];
            # Workspace look mirrored from the MacBook profile. Applied to
            # zen-sessions.jsonlz4 on activation — only while Zen is closed.
            # Only Main exists, as on the MacBook — undeclared spaces are deleted.
            spacesForce = true;
            spaces.Main = {
              id = "045a59e3-f218-48f4-b608-f6a391657b43";
              position = 0; # keep it first (module default 1000 would push it last)
              icon = "chrome://browser/skin/zen-icons/selectable/coins.svg";
              theme = {
                opacity = 0.555;
                colors =
                  map
                    (c: {
                      inherit (c) red green blue primary;
                      algorithm = "analogous";
                      lightness = 10;
                      type = "explicit-lightness";
                      position = { inherit (c) x y; };
                    })
                    [
                      { red = 19; green = 9; blue = 42; primary = true; x = 165; y = 112; }
                      { red = 42; green = 9; blue = 38; primary = false; x = 221; y = 125; }
                      { red = 9; green = 26; blue = 42; primary = false; x = 119; y = 147; }
                    ];
              };
            };
            extraConfig = ''
              ${builtins.readFile "${inputs.betterfox}/Fastfox.js"}
              ${builtins.readFile "${inputs.betterfox}/Peskyfox.js"}
              ${builtins.readFile "${inputs.betterfox}/Securefox.js"}
              ${builtins.readFile "${inputs.betterfox}/Smoothfox.js"}
              lockPref("extensions.formautofill.addresses.enabled", false);
              lockPref("extensions.formautofill.creditCards.enabled", false);
              lockPref("dom.security.https_only_mode_pbm", true);
              lockPref("dom.security.https_only_mode_error_page_user_suggestions", true);
              lockPref("browser.firefox-view.feature-tour", "{\"screen\":\"\",\"complete\":true}");
              lockPref("identity.fxaccounts.enabled", false);
              lockPref("browser.tabs.firefox-view-next", false);
              lockPref("privacy.sanitize.sanitizeOnShutdown", false);
              lockPref("privacy.clearOnShutdown.cache", true);
              lockPref("privacy.clearOnShutdown.cookies", false);
              lockPref("privacy.clearOnShutdown.offlineApps", false);
              lockPref("browser.sessionstore.privacy_level", 0);
              lockPref("floorp.browser.sidebar.enable", false);
              lockPref("geo.enabled", false);
              lockPref("media.navigator.enabled", false);
              lockPref("dom.event.clipboardevents.enabled", false);
              lockPref("dom.event.contextmenu.enabled", false);
              lockPref("dom.battery.enabled", false);
              lockPref("extensions.enabledScopes", 15);
              lockPref("extensions.autoDisableScopes", 0);
              lockPref("browser.newtabpage.activity-stream.floorp.newtab.imagecredit.hide", true);
              lockPref("browser.newtabpage.activity-stream.floorp.newtab.releasenote.hide", true);
              lockPref("browser.search.separatePrivateDefault", true);
            '';
          };
        };
      };
    })
  ];
}
