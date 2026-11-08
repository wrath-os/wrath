# A system built from the lowered model without NixOS: the kernel, an initrd, /etc, units, users
# and packages, put together from nixpkgs packages.
#
# `layers` add what the model doesn't describe yet, such as the test harness's agent or how a test
# VM mounts its root. Each is an attribute set with any of these, and they add up:
#
#   units           name to { text, wanted_by, required_by }, like the model's units
#   etc             path under /etc to { text }
#   packages        packages for the system's environment
#   kernelModules   modules loaded once booted
#   kernelParams    the kernel's command line
#   services        packages whose units, udev rules and D-Bus policy join the system
#   enable          upstream units to enable, as `systemctl enable` does
#   initrd          { units, fstab, kernelModules, storePaths }, for the initrd
#
# Parts of the model this doesn't build yet are refused, so nothing is silently left out.
{
  lib,
  pkgs,
  input,
  package,
  distro,
  layers ? [ ],
}:

let
  base = input.base;
  inherit (pkgs) systemd;
  allLayers = [ (import ./base.nix { inherit lib pkgs base; }) ] ++ layers;

  unsupported =
    lib.optional (input.secrets != { }) "secrets"
    ++ lib.optional (input.templates != { }) "files with secrets"
    ++ lib.optional (input.homes != { }) "homes"
    ++ lib.optional (input.triggers != { }) "reload and restart triggers"
    ++ lib.optional (input.nixos != { }) "building on NixOS"
    ++ lib.optional (base ? storage) "disks"
    ++ lib.optional (base ? session) "a graphical session"
    ++ lib.optional base.audio.enable "audio";

  # What the layers give at a dotted `path`, such as `initrd.fstab`: lists concatenated, attribute
  # sets merged with later layers winning.
  fromLayers = default: path: map (lib.attrByPath (lib.splitString "." path) default) allLayers;
  sum = path: lib.concatLists (fromLayers [ ] path);
  union = path: lib.mergeAttrsList (fromLayers { } path);

  units = input.units // union "units";
  etcFiles = lib.mapAttrs (_: file: { inherit (file) text; }) input.etc // union "etc";

  kernel = pkgs.linuxPackages.kernel;
  moduleTree = pkgs.aggregateModules [ (lib.getOutput "modules" kernel) ];

  # The packages everyone expects on a system, then the model's.
  basePackages = [
    pkgs.bashInteractive
    pkgs.coreutils
    pkgs.findutils
    pkgs.gawk
    pkgs.gnugrep
    pkgs.gnused
    pkgs.iproute2
    pkgs.iputils
    pkgs.kmod
    pkgs.less
    pkgs.procps
    pkgs.shadow
    pkgs.util-linux
    systemd
  ];
  packages = basePackages ++ [ distro ] ++ map package input.packages ++ sum "packages";
  sw = pkgs.buildEnv {
    name = "system-path";
    paths = packages;
    ignoreCollisions = true;
    pathsToLink = [
      "/bin"
      "/sbin"
      "/lib"
      "/share"
    ];
  };

  # Packages whose units, users, tmpfiles, udev rules and D-Bus policy join the system, such as
  # dbus. Other packages only add their programs.
  services = [ systemd ] ++ sum "services";
  # Where `services` keep `dir`, under any of `prefixes`, with systemd's examples first.
  shipped =
    prefixes: dir:
    [ "${systemd}/example/${dir}" ]
    ++ lib.concatMap (p: map (prefix: "${p}/${prefix}/${dir}") prefixes) services;
  # A directory linking every file in `dirs`, later ones replacing earlier ones.
  merged =
    name: dirs:
    pkgs.runCommand name { } ''
      mkdir $out
      for dir in ${lib.escapeShellArgs dirs}; do
        [ -d "$dir" ] || continue
        for file in "$dir"/*; do
          if [ -e "$file" ]; then ln -sfn "$file" "$out/$(basename "$file")"; fi
        done
      done
    '';

  # Units systemd and the service packages ship, with their links that make targets want them,
  # overlaid by the model's. Upstream units in `enable` are enabled as `systemctl enable` does,
  # which follows their [Install] sections.
  unitDir =
    upstream: own: enable:
    pkgs.runCommand "units" { } ''
      mkdir $out
      for dir in ${lib.escapeShellArgs upstream}; do
        [ -d "$dir" ] || continue
        for unit in "$dir"/*; do
          name=$(basename "$unit")
          if [ -d "$unit" ] && [[ "$name" == *.wants || "$name" == *.requires ]]; then
            mkdir -p "$out/$name"
            for link in "$unit"/*; do
              ln -sfn "../$(basename "$link")" "$out/$name/$(basename "$link")"
            done
          else
            ln -sfn "$unit" "$out/$name"
          fi
        done
      done
      ${writeUnits own}
      ${lib.optionalString (enable != [ ]) ''
        mkdir -p root/etc/systemd
        cp -rL $out root/etc/systemd/system
        chmod -R u+w root
        ${systemd}/bin/systemctl --root=root enable ${lib.escapeShellArgs enable}
        cd root/etc/systemd/system
        find . -type l | while read -r link; do
          mkdir -p "$out/$(dirname "$link")"
          cp -P "$link" "$out/$link"
        done
      ''}
    '';
  unitDirs = shipped [
    "lib"
    "etc"
  ];
  systemUnits = unitDir (unitDirs "systemd/system") units (sum "enable");
  userUnits = unitDir (unitDirs "systemd/user") { } [ ];

  # udev's hardware database, built from the service packages' sources.
  hwdb = pkgs.runCommand "hwdb" { } ''
    mkdir -p etc/udev/hwdb.d $out
    for dir in ${lib.escapeShellArgs (shipped [ "lib" ] "udev/hwdb.d")}; do
      [ -d "$dir" ] || continue
      cp -fL "$dir"/* etc/udev/hwdb.d/
    done
    ${systemd}/bin/systemd-hwdb --root="$PWD" update
    cp etc/udev/hwdb.bin $out/hwdb.bin
  '';

  # The system bus: dbus's own configuration, reading policy from /etc/dbus-1/system.d and
  # starting the services the service packages ship.
  dbusConfig = pkgs.runCommand "system.conf" { } ''
    sed \
      -e '\#<include ignore_missing="yes">/etc/dbus-1/system.conf</include>#d' \
      -e 's#<includedir>system.d</includedir>##' \
      -e 's#</busconfig>#${
        lib.concatMapStrings (
          p: "<servicedir>${p}/share/dbus-1/system-services</servicedir>"
        ) services
      }</busconfig>#' \
      ${pkgs.dbus}/share/dbus-1/system.conf > $out
  '';

  users = import ./users.nix { inherit lib pkgs; } base.users;
  writeUnits = import ./units.nix { inherit lib pkgs; };

  osRelease = ''
    NAME=distro
    ID=distro
    PRETTY_NAME=distro
  '';

  etc = pkgs.runCommand "etc" { } ''
    mkdir -p $out/systemd $out/udev $out/dbus-1
    ln -s ${systemUnits} $out/systemd/system
    ln -s ${userUnits} $out/systemd/user
    ln -s ${merged "udev-rules" (shipped [ "lib" ] "udev/rules.d")} $out/udev/rules.d
    ln -s ${hwdb}/hwdb.bin $out/udev/hwdb.bin
    ln -s ${merged "dbus-system" (shipped [ "share" ] "dbus-1/system.d")} $out/dbus-1/system.d
    ln -s ${dbusConfig} $out/dbus-1/system.conf
    ${lib.concatMapStrings (dir: ''
      mkdir $out/${dir}
      for file in ${merged dir (shipped [ "lib" ] dir)}/*; do
        if [ -e "$file" ]; then ln -s "$file" $out/${dir}/; fi
      done
    '') [ "binfmt.d" "modules-load.d" "sysctl.d" "sysusers.d" "tmpfiles.d" ]}
    ln -s ${pkgs.writeText "os-release" osRelease} $out/os-release
    ln -s ${pkgs.tzdata}/share/zoneinfo/${base.system.timezone} $out/localtime
    ln -s ${pkgs.writeText "nsswitch.conf" ''
      passwd: files systemd
      group: files systemd
      shadow: files
      hosts: files myhostname dns
    ''} $out/nsswitch.conf
    ln -s ${pkgs.writeText "profile" ''
      export PATH=/run/wrappers/bin:/run/current-system/sw/bin
    ''} $out/profile
    ln -s ${pkgs.writeText "modules.conf" (lib.concatLines (sum "kernelModules"))} \
      $out/modules-load.d/distro.conf
    ln -s ${pkgs.writeText "files.conf" (
      lib.concatStrings (
        lib.mapAttrsToList (
          path: file: "L+ ${path} - - - - ${pkgs.writeText (baseNameOf path) file.text}\n"
        ) input.files
      )
    )} $out/tmpfiles.d/distro.conf
    ${lib.optionalString (base.system.hostname != "") ''
      echo ${lib.escapeShellArg base.system.hostname} > $out/hostname
    ''}
    cp ${users.passwd} $out/passwd
    cp ${users.group} $out/group
    cp ${users.shadow} $out/shadow
    ${lib.concatStrings (
      lib.mapAttrsToList (path: file: ''
        mkdir -p "$(dirname "$out/${path}")"
        ln -sfn ${pkgs.writeText (baseNameOf path) file.text} "$out/${path}"
      '') etcFiles
    )}
    mkdir -p $out/distro
    ln -s ${pkgs.writeText "model.json" input.model} $out/distro/model.json
    ln -s ${pkgs.writeText "model-hash" input.model_hash} $out/distro/model-hash
    mkdir -p $out/nix
    ln -s ${pkgs.writeText "nix.conf" (lib.concatLines (sum "nixSettings"))} $out/nix/nix.conf
  '';

  initrd = import ./initrd.nix {
    inherit lib pkgs kernel moduleTree writeUnits;
    hostname = base.system.hostname;
    units = union "initrd.units";
    fstab = sum "initrd.fstab";
    kernelModules = sum "initrd.kernelModules";
    storePaths = sum "initrd.storePaths";
  };

  kernelParams = base.boot.kernel_params ++ sum "kernelParams";

  toplevel = pkgs.runCommand "distro-system" { } ''
    mkdir $out
    ln -s ${kernel}/bzImage $out/kernel
    ln -s ${moduleTree} $out/kernel-modules
    ln -s ${initrd}/initrd $out/initrd
    ln -s ${etc} $out/etc
    ln -s ${sw} $out/sw
    ln -s ${systemd} $out/systemd
    echo "init=$out/init ${lib.concatStringsSep " " kernelParams}" > $out/kernel-params
    substitute ${./init.sh} $out/init \
      --subst-var-by bash ${pkgs.bash} \
      --subst-var-by path ${
        lib.makeBinPath [
          pkgs.coreutils
          pkgs.findutils
          pkgs.util-linux
        ]
      } \
      --subst-var-by systemd ${systemd} \
      --subst-var out
    chmod +x $out/init
    # How distro switches to this system: `switch`, `test` or `boot`.
    mkdir $out/bin
    cat > $out/bin/switch-to-configuration <<EOF
    #!${pkgs.runtimeShell}
    exec ${distro}/bin/distro activate --mode "\$1" $out
    EOF
    chmod +x $out/bin/switch-to-configuration
  '';
in
if unsupported != [ ] then
  throw "distro: the native lowering doesn't build ${lib.concatStringsSep ", " unsupported} yet"
else
  toplevel
