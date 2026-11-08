# What the native lowering itself puts on every system: systemd, which it assembles the system
# around, and Nix, which distro builds and switches with. The rest of the base system comes from
# distro's base modules.
{ pkgs }:

{
  packages = [
    pkgs.systemd
    pkgs.nix
  ];

  nixSettings = [
    "experimental-features = nix-command flakes"
    "build-users-group ="
  ];
}
