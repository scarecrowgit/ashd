{ ... }:

{
  fileSystems."/mnt/storage" = {
    device = "/dev/disk/by-uuid/7740c17a-4306-4418-b825-b8fa4c8ebf60";
    fsType = "ext4";
    options = [ "defaults" "nofail" ];
  };

  fileSystems."/mnt/archive" = {
    device = "/dev/disk/by-label/archive";
    fsType = "ext4";
    options = [ "defaults" "nofail" ];
  };
}
