# What test systems add so the harness can drive them: the guest agent on vsock, a share from
# the host for copying in new systems, and a root shell on the serial console. The disk layout
# comes from `test-disk.nix` or `test-host-store.nix`.
{
  agent,
  lib,
  ...
}:

{
  # The run directory's `share` on the host, when the harness passes one. Overrides like the
  # disk layout's, so it's merged with them.
  fileSystems = lib.mkOverride 10 {
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

  # Store paths copied in from the host's share carry no signatures.
  nix.settings.require-sigs = false;

  # The harness gets in through the agent, so nobody needs a password.
  users.allowNoPasswordLogin = true;

  # Test images only: a root shell on the serial console for interactive debugging.
  services.getty.autologinUser = "root";

  documentation.enable = false;
}
