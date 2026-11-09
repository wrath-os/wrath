# A plain NixOS system for testing the harness itself, the one NixOS system in the factory.
{
  imports = [
    ./test-harness.nix
    ./test-disk.nix
    (import ./test-share.nix 10)
  ];

  system.stateVersion = "26.05";
  networking.hostName = "harness-fixture";
}
