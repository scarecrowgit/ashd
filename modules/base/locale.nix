{ hostVars, lib, ... }:

{
  time.timeZone = lib.mkDefault hostVars.timeZone;
  i18n.defaultLocale = lib.mkDefault hostVars.locale;
}
