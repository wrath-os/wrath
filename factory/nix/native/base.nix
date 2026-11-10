# What the native lowering itself puts on every system: systemd, which it assembles the system
# around, and Nix, which distro builds and switches with. The rest of the base system comes from
# distro's base modules.
{ pkgs }:

{
  packages = [
    pkgs.systemd
    pkgs.nix
  ];

  # Users build and fetch through the daemon, since only root writes to the store directly.
  services = [ pkgs.nix ];
  enabled."nix-daemon.socket" = [ ];

  nixSettings = [
    "experimental-features = nix-command flakes"
    "build-users-group ="
  ];

  # Programs from nixpkgs look for graphics drivers under /run/opengl-driver.
  etc."tmpfiles.d/opengl-driver.conf".text =
    "L+ /run/opengl-driver - - - - /run/current-system/sw\n";
}
