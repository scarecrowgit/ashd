{ ... }:

{
  home-manager.sharedModules = [
    (_: {
      programs.gh = {
        enable = true;
        # Also registers `gh auth git-credential` as git's credential helper for github.com
        gitCredentialHelper.enable = true;
        settings = {
          git_protocol = "ssh";
          prompt = "enabled";
        };
      };
    })
  ];
}
