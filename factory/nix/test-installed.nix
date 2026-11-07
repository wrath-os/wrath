# What systems installed onto a VM's disk by `distro install` add to the harness module: the virtio
# drivers to find their disks. Their disk layout is the installed configuration's own.
{ modulesPath, ... }:

{
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];
}
