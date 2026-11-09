# The run directory's `share` on the host, when the harness passes one. `priority` is that of the
# disk layout it joins, so the share goes in at the same priority as the rest of it.
priority:
{ lib, ... }:

{
  fileSystems = lib.mkOverride priority {
    "/run/distro-share" = {
      device = "distro-share";
      fsType = "9p";
      options = [
        "trans=virtio"
        "version=9p2000.L"
        "msize=1048576"
        "ro"
        "nofail"
      ];
    };
  };
}
