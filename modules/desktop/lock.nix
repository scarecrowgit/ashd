# ashd-lock authenticates through PAM service "ashd-lock".
{ config, lib, ... }:

{
  config = lib.mkIf config.ashd.enable {
    security.pam.services.ashd-lock = { };
  };
}
