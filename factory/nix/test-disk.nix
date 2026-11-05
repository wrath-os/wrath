# The disk layout of test images: a UEFI-bootable disk with the system's whole closure on it.
{ lib, modulesPath, ... }:

let
  # Above anything a host sets, so a test image of an existing machine gets the image's disk
  # instead of the machine's disks, encryption and swap.
  vm = lib.mkOverride 10;
in

{
  imports = [
    # virtio drivers in the initrd, so the root disk is found.
    (modulesPath + "/profiles/qemu-guest.nix")
    # The disk layout of the image, so systems switched to later mount the same filesystems.
    (modulesPath + "/virtualisation/disk-image.nix")
  ];

  fileSystems = vm {
    "/" = {
      device = "/dev/disk/by-label/nixos";
      fsType = "ext4";
      autoResize = true;
    };
    "/boot" = {
      device = "/dev/disk/by-label/ESP";
      fsType = "vfat";
    };
  };
  swapDevices = vm [ ];
  boot.initrd.luks.devices = vm { };
}
