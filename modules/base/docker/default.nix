{ hostVars, pkgs, ... }:

{
  virtualisation.docker = {
    enable = true;
    daemon.settings = {
      data-root = "/mnt/storage/docker";
    };
    # Reclaim disk space from dangling images/containers/volumes automatically
    autoPrune = {
      enable = true;
      dates = "weekly";
    };
  };

  users.users.${hostVars.userName}.extraGroups = [ "docker" ];

  environment.systemPackages = with pkgs; [
    docker-compose
  ];
}
