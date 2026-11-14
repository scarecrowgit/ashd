# Overlay to bump Discord stable past the version shipped in the pinned nixpkgs.
#
# nixpkgs pins Discord's release in sources.json, which lags slightly behind the
# live "stable" build, so the packaged client reports itself as outdated. This
# overlay overrides the `discord` package with the current upstream distro
# sources (1.0.158) and all its native modules.
#
# To refresh, read Discord's own update manifest (nixpkgs' sources.json can lag too):
#   https://discord.com/api/updates/distributions/app/manifests/latest?channel=stable&platform=linux&arch=x64
# `full.host_version` is the version; each `package_sha256` (hex) converts to SRI with
# `nix hash convert --hash-algo sha256 --to sri <hex>`. Keep the module list in sync with it.
final: prev:
let
  v = "1.0.158";
  base = "https://stable.dl2.discordapp.net/distro/app/stable/linux/x64/${v}";
  source = {
    version = v;
    kind = "distro";
    distro = {
      hash = "sha256-c+nKdZnTtTwc9aafZm2jW1sEAtlVRi8uh0iNWUQOlm0=";
      url = "${base}/full.distro";
    };
    modules = {
      discord_cloudsync = { hash = "sha256-RmuyIeOlDBLfzDjJ2jUB3c5ZVSErcvHCW2unLUHg0ns="; url = "${base}/discord_cloudsync/1/full.distro"; version = 1; };
      discord_desktop_core = { hash = "sha256-uzPT1BLb0NzGDJWKXwLHO8RThGOtY8NFCKnG0EJ/4Xw="; url = "${base}/discord_desktop_core/1/full.distro"; version = 1; };
      discord_dispatch = { hash = "sha256-6zH5+89RLni0Li0sdJIaIjLn5CogDr73P32D5B1F3wQ="; url = "${base}/discord_dispatch/1/full.distro"; version = 1; };
      discord_erlpack = { hash = "sha256-+arjlg73Tx51zHLivza9q/+WJyht4zejAE+w2yzEAbc="; url = "${base}/discord_erlpack/1/full.distro"; version = 1; };
      discord_game_utils = { hash = "sha256-K0Y7ab7BF6G3AfA3ANe9oc8I5mCQdEztmrygpLxPfJY="; url = "${base}/discord_game_utils/1/full.distro"; version = 1; };
      discord_krisp = { hash = "sha256-O8X2GPqT2gJhEJUYsER70LbF+jiBCvDmRCNlEC5nkmo="; url = "${base}/discord_krisp/1/full.distro"; version = 1; };
      discord_modules = { hash = "sha256-tcUy0Ii3QmhJPueMVnaCl57Tej0e5669Uf2cRXVbsQQ="; url = "${base}/discord_modules/1/full.distro"; version = 1; };
      discord_rpc = { hash = "sha256-wyuarTe5JtAcw8UmmtZekg5YD3V0raUAUr8j/AzH13Q="; url = "${base}/discord_rpc/1/full.distro"; version = 1; };
      discord_spellcheck = { hash = "sha256-isopidcTyr58OljFu/w7GiB/2i04Gdlh126p8b0/QxQ="; url = "${base}/discord_spellcheck/1/full.distro"; version = 1; };
      discord_sysimg = { hash = "sha256-5kRFWLgOCGcw+ZJIkmPL2wkeT09lBolUAZpdKTZdJZk="; url = "${base}/discord_sysimg/1/full.distro"; version = 1; };
      discord_utils = { hash = "sha256-X+bOVYfdRmKpU1K8/xIgzEpHrO/Vsj3DsRhG+jaAkwM="; url = "${base}/discord_utils/1/full.distro"; version = 1; };
      discord_voice = { hash = "sha256-816qNKzW1rFlUrjZaA9GpgbG0WSlUI9Eer7xNFRfSVY="; url = "${base}/discord_voice/1/full.distro"; version = 1; };
      discord_zstd = { hash = "sha256-qAaUpZStaaHtE+kIOm9k1T8i4V73ZFNrEXhL9OOwMdk="; url = "${base}/discord_zstd/1/full.distro"; version = 1; };
    };
  };
in
{
  discord = prev.discord.override { inherit source; };
}
