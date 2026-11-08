# The initrd: systemd, which mounts the root under /sysroot and switches to it. What the root is
# and how it's mounted come from `fstab` and `units`; everything under /sysroot in `fstab` is
# mounted before switching. With `disks`, it finds disks with udev and unlocks those in
# `crypttab`, asking for a passphrase on the console, or with the TPM.
{
  lib,
  pkgs,
  kernel,
  moduleTree,
  writeUnits,
  hostname,
  units,
  fstab,
  kernelModules,
  storePaths,
  disks,
  crypttab,
  udevRules,
  dlopenFeatures,
  ssh,
}:

let
  inherit (pkgs) systemd;
  # The util-linux systemd runs, such as for mount units.
  utilLinux = lib.getBin systemd.util-linux;
  tpm = lib.elem "tpm" dlopenFeatures;
  # Unlocking over SSH: networkd brings up wired interfaces with DHCP, and sshd lets in the keys
  # given, as root, to answer the passphrase prompt. Its host key comes from the machine's disk.
  withSsh = ssh != { };
  sshd = {
    text = ''
      [Unit]
      Description=SSH, to unlock the disks
      DefaultDependencies=no
      After=network.target
      Before=shutdown.target
      Conflicts=shutdown.target

      [Service]
      ExecStart=${pkgs.openssh}/bin/sshd -D -f /etc/ssh/sshd_config
      KillMode=process
      Restart=on-failure
    '';
    wanted_by = [ "initrd.target" ];
  };
  sshdConfig = ''
    Port ${toString ssh.port}
    HostKey /etc/ssh/initrd_ed25519_key
    AuthorizedKeysFile /etc/ssh/authorized_keys.d/%u
    UsePAM no
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    PermitRootLogin prohibit-password
  '';
  dhcp = ''
    [Match]
    Type=ether
    Kind=!*

    [Network]
    DHCP=yes
  '';

  # systemd's own units the initrd runs, from those it ships for every system.
  upstream = [
    "basic.target"
    "ctrl-alt-del.target"
    "emergency.service"
    "emergency.target"
    "final.target"
    "halt.target"
    "initrd-cleanup.service"
    "initrd-fs.target"
    "initrd-parse-etc.service"
    "initrd-root-device.target"
    "initrd-root-fs.target"
    "initrd-switch-root.service"
    "initrd-switch-root.target"
    "initrd.target"
    "kmod-static-nodes.service"
    "local-fs-pre.target"
    "local-fs.target"
    "modprobe@.service"
    "paths.target"
    "poweroff.target"
    "reboot.target"
    "rescue.service"
    "rescue.target"
    "shutdown.target"
    "slices.target"
    "sockets.target"
    "swap.target"
    "sysinit.target"
    "systemd-journald-dev-log.socket"
    "systemd-journald.service"
    "systemd-journald.socket"
    "systemd-modules-load.service"
    "systemd-poweroff.service"
    "systemd-reboot.service"
    "systemd-halt.service"
    "systemd-tmpfiles-setup-dev-early.service"
    "systemd-tmpfiles-setup-dev.service"
    "timers.target"
    "umount.target"
  ]
  ++ lib.optionals tpm [
    "systemd-pcrextend.socket"
    "systemd-pcrextend@.service"
    "systemd-tpm2-setup-early.service"
    "tpm2.target"
  ]
  ++ lib.optionals withSsh [
    "network-pre.target"
    "network.target"
    "systemd-networkd.service"
    "systemd-networkd.socket"
    "systemd-networkd-varlink.socket"
  ]
  ++ lib.optionals disks [
    "cryptsetup-pre.target"
    "cryptsetup.target"
    "initrd-udevadm-cleanup-db.service"
    "systemd-ask-password-console.path"
    "systemd-ask-password-console.service"
    "systemd-udev-trigger.service"
    "systemd-udevd-control.socket"
    "systemd-udevd-kernel.socket"
    "systemd-udevd.service"
  ];

  # The rules that name disks and partitions, and device-mapper's for unlocked containers.
  udev = pkgs.runCommand "initrd-udev-rules" { } ''
    mkdir $out
    for rule in 50-udev-default 60-block 60-persistent-storage 64-btrfs 80-drivers 99-systemd; do
      ln -s ${systemd}/lib/udev/rules.d/$rule.rules $out/
    done
    for rule in 10-dm 13-dm-disk 95-dm-notify; do
      ln -s ${pkgs.lvm2.out}/lib/udev/rules.d/$rule.rules $out/
    done
    cp ${pkgs.writeText "distro.rules" udevRules} $out/90-distro.rules
  '';
  example = "${systemd}/example/systemd/system";

  # systemd only switches to a root that says what it is, so the system's os-release goes in
  # first. The system is the directory of the `init=` on the kernel's command line; the booted
  # system's own init puts the rest of /etc in place.
  rootScript = pkgs.writeShellScript "distro-root" ''
    set -eu
    PATH=${pkgs.coreutils}/bin:${pkgs.gnused}/bin
    # A path in the new root, following its links as the new root will see them.
    resolve() {
      local out=/sysroot part target
      IFS=/ read -ra parts <<< "''${1#/}"
      for part in "''${parts[@]}"; do
        out="$out/$part"
        while [ -L "$out" ]; do
          target=$(readlink "$out")
          case "$target" in
            /*) out="/sysroot$target" ;;
            *) out="$(dirname "$out")/$target" ;;
          esac
        done
      done
      echo "$out"
    }
    init=$(sed -n 's/.*\binit=\([^ ]*\).*/\1/p' /proc/cmdline)
    # A root on disk has it already, as a link into the store, from the last boot.
    if [ ! -e "$(resolve /etc/os-release)" ]; then
      mkdir -p /sysroot/etc
      rm -f /sysroot/etc/os-release
      cp "$(resolve "''${init%/init}/etc/os-release")" /sysroot/etc/os-release
    fi
  '';
  rootService = {
    text = ''
      [Unit]
      Description=Prepare the root to switch to
      DefaultDependencies=no
      RequiresMountsFor=/sysroot /sysroot/nix/store
      After=initrd-fs.target
      Before=initrd.target

      [Service]
      Type=oneshot
      RemainAfterExit=yes
      ExecStart=${rootScript}
    '';
    required_by = [ "initrd.target" ];
  };

  unitDir = pkgs.runCommand "initrd-units" { } ''
    mkdir -p $out/sysinit.target.wants
    ${lib.concatMapStrings (unit: ''
      ln -s ${example}/${unit} $out/${unit}
    '') upstream}
    for want in ${example}/sysinit.target.wants/*; do
      name=$(basename "$want")
      if [ -e "$out/$name" ]; then
        ln -s "../$name" "$out/sysinit.target.wants/$name"
      fi
    done
    ln -s initrd.target $out/default.target
    ${lib.optionalString withSsh ''
      mkdir -p $out/initrd.target.wants
      ln -s ../systemd-networkd.service $out/initrd.target.wants/
    ''}
    ${writeUnits (
      units
      // { "distro-root.service" = rootService; }
      // lib.optionalAttrs withSsh { "sshd.service" = sshd; }
    )}
  '';

  bin = pkgs.buildEnv {
    name = "initrd-bin";
    paths = [
      pkgs.bashInteractive
      pkgs.coreutils
      pkgs.kmod
      utilLinux
      systemd
    ];
    pathsToLink = [
      "/bin"
      "/sbin"
    ];
  };

  modules = pkgs.makeModulesClosure {
    kernel = moduleTree;
    firmware = pkgs.emptyDirectory;
    rootModules = kernelModules;
    # Drivers built into the kernel aren't modules to copy.
    allowMissing = true;
  };

  release = pkgs.writeText "initrd-release" ''
    NAME=distro
    ID=distro
    PRETTY_NAME="distro (initrd)"
  '';

  contents = [
    {
      source = "${systemd}/lib/systemd/systemd";
      target = "/init";
    }
    {
      source = unitDir;
      target = "/etc/systemd/system";
    }
    {
      source = pkgs.writeText "system.conf" ''
        [Manager]
        DefaultEnvironment=PATH=/bin:/sbin
        ManagerEnvironment=PATH=/bin:/sbin
      '';
      target = "/etc/systemd/system.conf";
    }
    {
      source = release;
      target = "/etc/initrd-release";
    }
    {
      source = release;
      target = "/etc/os-release";
    }
    {
      source = pkgs.writeText "fstab" (lib.concatLines fstab);
      target = "/etc/fstab";
    }
    {
      source = pkgs.writeText "shadow" "root:*:::::::\n";
      target = "/etc/shadow";
    }
    {
      # sshd runs its sessions apart as `sshd`, and networkd runs as its own user.
      source = pkgs.writeText "passwd" ''
        root:x:0:0:root:/root:/bin/sh
        sshd:x:74:74:sshd:/var/empty:/bin/false
        systemd-network:x:152:152:networkd:/:/bin/false
      '';
      target = "/etc/passwd";
    }
    {
      # The groups udev's rules give devices to.
      source = pkgs.writeText "group" (
        lib.concatLines (
          lib.imap1 (n: group: "${group}:x:${toString n}:") [
            "tty"
            "disk"
            "kmem"
            "input"
            "video"
            "audio"
            "lp"
            "dialout"
            "cdrom"
            "tape"
            "floppy"
            "render"
            "sgx"
            "clock"
            "kvm"
          ]
        )
        + "root:x:0:\nsshd:x:74:\nsystemd-network:x:152:\n"
      );
      target = "/etc/group";
    }
    {
      source = pkgs.writeText "modules.conf" (lib.concatLines kernelModules);
      target = "/etc/modules-load.d/distro.conf";
    }
    {
      source = "${modules}/lib";
      target = "/lib";
    }
    {
      source = "${bin}/bin";
      target = "/bin";
    }
    {
      source = "${bin}/sbin";
      target = "/sbin";
    }
  ]
  ++ lib.optionals disks [
    {
      source = udev;
      target = "/etc/udev/rules.d";
    }
    {
      source = pkgs.writeText "crypttab" (lib.concatLines crypttab);
      target = "/etc/crypttab";
    }
  ]
  ++ lib.optionals withSsh [
    {
      source = pkgs.writeText "sshd_config" sshdConfig;
      target = "/etc/ssh/sshd_config";
    }
    {
      source = pkgs.writeText "authorized_keys" (lib.concatLines ssh.authorizedKeys);
      target = "/etc/ssh/authorized_keys.d/root";
    }
    {
      source = pkgs.writeText "99-dhcp.network" dhcp;
      target = "/etc/systemd/network/99-dhcp.network";
    }
  ]
  # libcryptsetup unlocks with the TPM through systemd's plugin, which it looks for among its own.
  ++ lib.optional tpm {
    source = "${systemd}/lib/cryptsetup/libcryptsetup-token-systemd-tpm2.so";
    target = "${lib.getLib pkgs.cryptsetup}/lib/cryptsetup/libcryptsetup-token-systemd-tpm2.so";
  }
  ++ lib.optional (hostname != "") {
    source = pkgs.writeText "hostname" "${hostname}\n";
    target = "/etc/hostname";
  }
  ++ map (path: { source = path; }) (
    [
      "${systemd}/lib/systemd/systemd-executor"
      "${systemd}/lib/systemd/systemd-journald"
      "${systemd}/lib/systemd/systemd-modules-load"
      "${systemd}/lib/systemd/systemd-shutdown"
      "${systemd}/lib/systemd/systemd-sulogin-shell"
      "${systemd}/lib/systemd/systemd-sysroot-fstab-check"
      "${systemd}/lib/systemd/system-generators/systemd-fstab-generator"
      "${utilLinux}/bin/mount"
      "${utilLinux}/bin/umount"
      "${lib.getOutput "login" systemd.util-linux}/bin/sulogin"
      "${pkgs.glibc}/lib/libnss_files.so.2"
      rootScript
      # Scripts such as `rootScript` run with it.
      pkgs.runtimeShell
      "${pkgs.gnused}/bin/sed"
    ]
    ++ lib.optionals withSsh [
      "${pkgs.openssh}/bin/sshd"
      "${pkgs.openssh}/libexec/sshd-session"
      "${pkgs.openssh}/libexec/sshd-auth"
      "${systemd}/lib/systemd/systemd-networkd"
      "${systemd}/lib/systemd/systemd-sysctl"
    ]
    ++ lib.optionals tpm [
      pkgs.tpm2-tss
      "${systemd}/lib/systemd/systemd-tpm2-setup"
      "${systemd}/lib/systemd/systemd-pcrextend"
      "${systemd}/lib/systemd/system-generators/systemd-tpm2-generator"
    ]
    ++ lib.optionals disks [
      "${systemd}/lib/systemd/systemd-udevd"
      "${systemd}/lib/systemd/systemd-cryptsetup"
      "${systemd}/lib/systemd/system-generators/systemd-cryptsetup-generator"
      "${systemd}/lib/udev/ata_id"
      "${systemd}/lib/udev/scsi_id"
      "${pkgs.lvm2.bin}/bin/dmsetup"
    ]
    ++ storePaths
  );
in
pkgs.makeInitrdNG {
  name = "initrd-${kernel.name}";
  # systemd loads some libraries only when it needs them, such as libmount; their ELF notes say
  # which, and these are copied too.
  contents = map (
    entry:
    entry
    // {
      dlopen = {
        usePriority = "recommended";
        features = dlopenFeatures;
      };
    }
  ) contents;
}
