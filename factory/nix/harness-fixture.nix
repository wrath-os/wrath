# A plain NixOS system for testing the harness itself, before there is an OS of our own.
{ agent, modulesPath, ... }:

{
  # virtio drivers in the initrd, so the root disk is found.
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

  system.stateVersion = "26.05";
  networking.hostName = "harness-fixture";

  boot.kernelParams = [ "console=ttyS0" ];
  boot.kernelModules = [ "vmw_vsock_virtio_transport" ];

  systemd.services.distro-agent = {
    description = "distro test harness agent";
    wantedBy = [ "multi-user.target" ];
    # Commands from the harness see the same programs as a login shell.
    path = [
      "/run/wrappers"
      "/run/current-system/sw"
    ];
    serviceConfig = {
      ExecStart = "${agent}/bin/distro-agent";
      Restart = "always";
    };
  };

  # Test fixture only: a root shell on the serial console for interactive debugging.
  services.getty.autologinUser = "root";

  documentation.enable = false;
}
