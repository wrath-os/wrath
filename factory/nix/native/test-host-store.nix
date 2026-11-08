# Test VMs that boot the kernel directly and mount the host's store over 9p. The root and the
# store's writable layer are in memory and start empty each boot.
{ pkgs }:

let
  ninep = "trans=virtio,version=9p2000.L,msize=1048576";
in
{
  initrd = {
    kernelModules = [
      "virtio_pci"
      "9p"
      "9pnet_virtio"
      "overlay"
    ];
    fstab = [
      "tmpfs /sysroot tmpfs mode=0755 0 0"
      "nix-store /sysroot/nix/.ro-store 9p ${ninep},cache=loose,ro 0 0"
      ("overlay /sysroot/nix/store overlay lowerdir=/sysroot/nix/.ro-store,"
        + "upperdir=/sysroot/nix/.rw-store/store,workdir=/sysroot/nix/.rw-store/work,"
        + "x-systemd.requires=distro-rw-store.service,"
        + "x-systemd.requires-mounts-for=/sysroot/nix/.ro-store 0 0")
    ];
    units."distro-rw-store.service" = {
      text = ''
        [Unit]
        Description=The store's writable layer
        DefaultDependencies=no
        RequiresMountsFor=/sysroot
        # The store stays mounted through the switch, so this stays up with it.
        IgnoreOnIsolate=yes
        Before=sysroot-nix-store.mount

        [Service]
        Type=oneshot
        RemainAfterExit=yes
        ExecStart=${pkgs.coreutils}/bin/mkdir -p \
          /sysroot/nix/.rw-store/store /sysroot/nix/.rw-store/work
      '';
    };
    storePaths = [ "${pkgs.coreutils}/bin/mkdir" ];
  };

  # The store holds the host's paths without knowing them; registering them lets Nix copy and
  # build on top, and the booted system becomes the first generation.
  units."distro-register-store.service" = {
    text = ''
      [Unit]
      Description=Register the host's store paths
      DefaultDependencies=no
      After=local-fs.target
      Before=sysinit.target

      [Service]
      Type=oneshot
      RemainAfterExit=yes
      ExecStart=${pkgs.writeShellScript "distro-register-store" ''
        set -eu
        registration=$(${pkgs.gnused}/bin/sed -n 's/.*\bregInfo=\([^ ]*\).*/\1/p' /proc/cmdline)
        ${pkgs.nix}/bin/nix-store --load-db < "$registration"
        ${pkgs.nix}/bin/nix-env --profile /nix/var/nix/profiles/system \
          --set "$(${pkgs.coreutils}/bin/readlink /run/current-system)"
      ''}
    '';
    wanted_by = [ "sysinit.target" ];
  };
}
