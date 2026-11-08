# The initrd: systemd, which mounts the root under /sysroot and switches to it. What the root is
# and how it's mounted come from `fstab` and `units`; everything under /sysroot in `fstab` is
# mounted before switching.
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
}:

let
  inherit (pkgs) systemd;
  # The util-linux systemd runs, such as for mount units.
  utilLinux = lib.getBin systemd.util-linux;

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
  ];
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
    mkdir -p /sysroot/etc
    cp "$(resolve "''${init%/init}/etc/os-release")" /sysroot/etc/os-release
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
    ${writeUnits (units // { "distro-root.service" = rootService; })}
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
    allowMissing = false;
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
        features = [ ];
      };
    }
  ) contents;
}
