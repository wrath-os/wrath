# Test VMs that boot the kernel directly and mount the host's store, so a large system needs no
# disk image. The root and the store's writable layer are in memory and start empty each boot.
{
  lib,
  modulesPath,
  pkgs,
  ...
}:

{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];

  virtualisation.diskImage = null;

  # The harness boots the kernel directly and there is no ESP, so switching installs nothing.
  system.build.installBootLoader = lib.mkForce "${pkgs.coreutils}/bin/true";

  # The harness runs QEMU itself and shares only the store, over 9p like the harness's own
  # share, so no virtiofsd is needed.
  virtualisation.sharedDirectories = lib.mkForce { };
  virtualisation.fileSystems."/nix/.ro-store" = {
    device = "nix-store";
    fsType = "9p";
    neededForBoot = true;
    options = [
      "trans=virtio"
      "version=9p2000.L"
      "msize=1048576"
      "cache=loose"
      "ro"
    ];
  };
  boot.initrd.availableKernelModules = [
    "9p"
    "9pnet_virtio"
  ];
}
