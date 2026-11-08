# What every native system has, from the model's base settings: consoles, networking and udev.
# These move into distro modules as the base system does.
{
  lib,
  pkgs,
  base,
}:

let
  inherit (pkgs) systemd;
  agetty = "${pkgs.util-linux}/bin/agetty --login-program ${pkgs.shadow}/bin/login";
  dhcp = base.networking.dhcp;
in
{
  units = {
    "getty@.service.d/distro.conf".text = ''
      [Service]
      ExecStart=
      ExecStart=-${agetty} --noreset --noclear - ''${TERM}
    '';
    "modprobe@.service.d/distro.conf".text = ''
      [Service]
      ExecSearchPath=${pkgs.kmod}/bin
    '';
    "serial-getty@.service.d/distro.conf".text = ''
      [Service]
      ExecStart=
      ExecStart=-${agetty} --noreset --noclear --keep-baud 115200,57600,38400,9600 - ''${TERM}
    '';
  };

  # Upstream units to enable, as `systemctl enable` would.
  enable =
    lib.optional (dhcp || base.networking.resolved) "systemd-networkd.service"
    ++ lib.optional base.networking.resolved "systemd-resolved.service";

  services = [ pkgs.dbus ];

  etc = {
    # Every boot of a root in memory looks like the first; nothing is enabled behind the
    # configuration's back.
    "systemd/system-preset/00-distro.preset".text = "ignore *\n";
    # Services find the system's programs, as a login shell does.
    "systemd/system.conf.d/distro.conf".text = ''
      [Manager]
      ManagerEnvironment=PATH=/run/wrappers/bin:/run/current-system/sw/bin
      DefaultEnvironment=PATH=/run/wrappers/bin:/run/current-system/sw/bin
    '';
    # The kernel loads modules it needs with this.
    "sysctl.d/50-distro.conf".text = "kernel.modprobe = /run/current-system/sw/bin/modprobe\n";
  }
  // lib.optionalAttrs dhcp {
    "systemd/network/99-dhcp.network".text = ''
      [Match]
      Type=ether
      Kind=!*

      [Network]
      DHCP=yes
    '';
  };

  packages = [ systemd ];
}
