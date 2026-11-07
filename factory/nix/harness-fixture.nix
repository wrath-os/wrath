# A plain NixOS system for testing the harness itself, before there is an OS of our own.
{
  imports = [
    ./test-harness.nix
    ./test-disk.nix
    (import ./test-share.nix 10)
  ];

  system.stateVersion = "26.05";
  networking.hostName = "harness-fixture";
}
