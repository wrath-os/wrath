# What native test systems add so the harness can drive them: the guest agent on vsock, the
# harness's share, and a root shell on the serial console.
{ pkgs, agent }:

{
  kernelParams = [
    "console=ttyS0"
    "systemd.journald.forward_to_console=1"
  ];
  kernelModules = [
    "vmw_vsock_virtio_transport"
    "9p"
    "9pnet_virtio"
  ];

  units."distro-agent.service" = {
    text = ''
      [Unit]
      Description=distro test harness agent

      [Service]
      Environment=PATH=/run/wrappers/bin:/run/current-system/sw/bin
      ExecStart=${agent}/bin/distro-agent
      Restart=always
    '';
    wanted_by = [ "multi-user.target" ];
  };

  units."run-distro\\x2dshare.mount" = {
    text = ''
      [Unit]
      Description=The harness's share
      DefaultDependencies=no
      Before=local-fs.target

      [Mount]
      What=distro-share
      Where=/run/distro-share
      Type=9p
      Options=trans=virtio,version=9p2000.L,msize=1048576,ro,nofail
    '';
    wanted_by = [ "local-fs.target" ];
  };

  # Test systems only: root logs in on the serial console without a password.
  units."serial-getty@ttyS0.service.d/autologin.conf" = {
    text = ''
      [Service]
      ExecStart=
      ExecStart=-${pkgs.util-linux}/bin/agetty --login-program ${pkgs.shadow}/bin/login --autologin root --keep-baud 115200,57600,38400,9600 %I $TERM
    '';
  };
}
