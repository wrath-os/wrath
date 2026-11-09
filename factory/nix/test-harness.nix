# What the harness fixture adds so the harness can drive it: the guest agent on vsock and a root
# shell on the serial console. The disk layout comes from `test-disk.nix` and the share from
# `test-share.nix`.
{ agent, ... }:

{
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

  # A root shell on the serial console for interactive debugging.
  services.getty.autologinUser = "root";

  documentation.enable = false;
}
