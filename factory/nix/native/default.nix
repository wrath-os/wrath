# A system built from the lowered model: the kernel, an initrd, /etc, units, users
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
#   enabled         upstream units to enable, each with the targets that want it, or none
#   initrd          { units, fstab, kernelModules, storePaths }, for the initrd
{
  lib,
  pkgs,
  input,
  package,
  distro,
  # What distro builds from, which the system keeps so it can build itself offline.
  factoryInputs ? [ ],
  layers ? [ ],
}:

let
  base = input.base;
  inherit (pkgs) systemd;
  allLayers = [
    (import ./base.nix { inherit pkgs; })
  ]
  ++ lib.optional (base ? storage) (
    import ./storage.nix {
      inherit lib pkgs;
      inherit (base) storage;
    }
  )
  ++ layers;

  # What the layers give at a dotted `path`, such as `initrd.fstab`: lists concatenated, attribute
  # sets merged with later layers winning.
  fromLayers = default: path: map (lib.attrByPath (lib.splitString "." path) default) allLayers;
  sum = path: lib.concatLists (fromLayers [ ] path);
  union = path: lib.mergeAttrsList (fromLayers { } path);

  units = input.units // union "units";
  etcFiles = lib.mapAttrs (_: file: { inherit (file) text; }) input.etc // union "etc";

  inherit (base) kernel;
  linux = (package kernel.package).kernel;
  # Where the kernel finds firmware, on its command line since it's asked for before /etc exists.
  firmware =
    if kernel.firmware == [ ] then
      pkgs.emptyDirectory
    else
      pkgs.buildEnv {
        name = "firmware";
        paths = map package kernel.firmware;
        pathsToLink = [ "/lib/firmware" ];
        ignoreCollisions = true;
      };
  moduleTree = pkgs.aggregateModules [ (lib.getOutput "modules" linux) ];

  packages = [
    distro
  ]
  ++ map package input.packages
  # nixpkgs keeps su in an output of its own, which distro's shadow has in it.
  ++ lib.optional (lib.elem "shadow" input.packages) pkgs.shadow.su
  ++ sum "packages";
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
  services = [ systemd ] ++ map package input.service_packages ++ sum "services";
  # Where `services` keep `dir`, under any of `prefixes`, with systemd's examples first.
  shipped =
    prefixes: dir:
    [ "${systemd}/example/${dir}" ]
    ++ lib.concatMap (p: map (prefix: "${p}/${prefix}/${dir}") prefixes) serviceOutputs;
  # Every output of the service packages, since a package's units can be in any of them.
  serviceOutputs = lib.concatMap (p: map (output: p.${output}) (p.outputs or [ "out" ])) services;
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

  # What packages other than systemd generate units with. systemd finds its own.
  generators = merged "system-generators" (
    lib.filter (dir: !lib.hasPrefix "${systemd}/" dir) (
      shipped [ "lib" ] "systemd/system-generators"
    )
  );

  # Units systemd and the service packages ship, with their links that make targets want them,
  # overlaid by the model's. Upstream units in `enabled` are wanted by the targets given for them,
  # or with none, enabled as `systemctl enable` does, by their [Install] sections. `kind` is
  # `system` or `user`; user units are enabled for every user.
  unitDir =
    kind: own: enabled:
    let
      byInstall = lib.attrNames (lib.filterAttrs (_: targets: targets == [ ]) enabled);
    in
    pkgs.runCommand "units" { } ''
      mkdir $out
      for dir in ${lib.escapeShellArgs (unitDirs "systemd/${kind}")}; do
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
      ${lib.concatStrings (
        lib.mapAttrsToList (
          unit: targets:
          lib.concatMapStrings (target: ''
            mkdir -p "$out/${target}.wants"
            ln -sfn "../${unit}" "$out/${target}.wants/${unit}"
          '') targets
        ) enabled
      )}
      ${lib.optionalString (byInstall != [ ]) ''
        mkdir -p root/etc/systemd
        cp -rL $out root/etc/systemd/${kind}
        chmod -R u+w root
        ${systemd}/bin/systemctl --root=root ${lib.optionalString (kind == "user") "--global"} \
          enable ${lib.escapeShellArgs byInstall}
        cd root/etc/systemd/${kind}
        find . -type l | while read -r link; do
          mkdir -p "$out/$(dirname "$link")"
          cp -P "$link" "$out/$link"
        done
      ''}
    '';
  unitDirs = shipped [
    "lib"
    "etc"
    "share"
  ];
  systemUnits = unitDir "system" units (input.enabled // union "enabled");
  userUnits = unitDir "user" { } input.enabled_user;

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
        ) serviceOutputs
      }</busconfig>#' \
      ${pkgs.dbus}/share/dbus-1/system.conf > $out
  '';
  # Users' session buses: dbus's own configuration, which finds services through XDG_DATA_DIRS.
  dbusSessionConfig = pkgs.runCommand "session.conf" { } ''
    sed \
      -e '\#<include ignore_missing="yes">/etc/dbus-1/session.conf</include>#d' \
      -e 's#<includedir>session.d</includedir>##' \
      ${pkgs.dbus}/share/dbus-1/session.conf > $out
  '';

  users = import ./users.nix { inherit lib pkgs; } base.users;

  # Where the machine fills in a file with secrets or machine values in it.
  filled = file: "/run/distro/templates/${file.name}";
  etcTemplates = lib.filterAttrs (path: _: lib.hasPrefix "/etc/" path) input.templates;
  otherTemplates = lib.filterAttrs (path: _: !(lib.hasPrefix "/etc/" path)) input.templates;
  templates =
    lib.attrValues input.templates
    ++ lib.concatMap (home: lib.attrValues (home.templates or { })) (lib.attrValues input.homes);

  homeLinks = import ../home-links.nix { inherit lib pkgs; };

  # What the machine does itself at every boot and switch, since it needs the machine's keys or
  # accounts: decrypting secrets, filling in files and linking homes.
  activation = pkgs.writeText "activation.json" (
    builtins.toJSON {
      age = "${pkgs.age}/bin/age";
      identities = base.secrets.identities or [ ];
      secrets = lib.mapAttrs (name: secret: {
        source = builtins.path {
          path = /. + secret.source;
          name = "${name}.age";
        };
        inherit (secret) owner mode link;
      }) input.secrets;
      templates_spec = pkgs.writeText "distro-templates.json" (
        builtins.toJSON (
          lib.listToAttrs (map (file: lib.nameValuePair file.name file.parts) templates)
        )
      );
      templates = map (file: { inherit (file) name owner mode; }) templates;
      hostname_filled = base.system.hostname == "";
      etc_modes = lib.mapAttrs (_: file: file.mode) (
        lib.filterAttrs (_: file: file.mode != "symlink") input.etc
      );
      bash = "${pkgs.bash}/bin/bash";
      link_home = "${../link-home.sh}";
      homes = lib.mapAttrsToList (name: user: {
        inherit name;
        inherit (user) group home;
        links = homeLinks name (input.homes.${name} or { }) filled;
      }) (lib.filterAttrs (_: user: !user.system) base.users);
    }
  );

  # Each user's own packages, where their programs are found.
  userProfiles = lib.filterAttrs (_: home: (home.packages or [ ]) != [ ]) input.homes;

  writeUnits = import ./units.nix { inherit lib pkgs; };

  osRelease = ''
    NAME=distro
    ID=distro
    PRETTY_NAME=distro
  '';

  linkInEtc = path: target: ''
    mkdir -p "$(dirname "$out/${path}")"
    ln -sfn ${target} "$out/${path}"
  '';

  etc = pkgs.runCommand "etc" { } ''
    mkdir -p $out/systemd $out/udev $out/dbus-1
    ln -s ${systemUnits} $out/systemd/system
    ln -s ${generators} $out/systemd/system-generators
    ln -s ${userUnits} $out/systemd/user
    # A directory of its own, so rules from the configuration can join what packages ship.
    mkdir $out/udev/rules.d
    ln -s ${merged "udev-rules" (shipped [ "lib" ] "udev/rules.d")}/* $out/udev/rules.d/
    ln -s ${hwdb}/hwdb.bin $out/udev/hwdb.bin
    ln -s ${merged "dbus-system" (shipped [ "share" "etc" ] "dbus-1/system.d")} $out/dbus-1/system.d
    ln -s ${dbusConfig} $out/dbus-1/system.conf
    ln -s ${dbusSessionConfig} $out/dbus-1/session.conf
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
      + lib.concatStrings (
        lib.mapAttrsToList (path: file: "L+ ${path} - - - - ${filled file}\n") otherTemplates
      )
    )} $out/tmpfiles.d/distro.conf
    ${lib.optionalString (base.system.hostname != "") ''
      echo ${lib.escapeShellArg base.system.hostname} > $out/hostname
    ''}
    cp ${users.passwd} $out/passwd
    cp ${users.group} $out/group
    cp ${users.shadow} $out/shadow
    ln -s ${users.homes} $out/tmpfiles.d/distro-homes.conf
    ${lib.concatStrings (
      lib.mapAttrsToList (
        path: file: linkInEtc path (pkgs.writeText (baseNameOf path) file.text)
      ) etcFiles
    )}
    ${lib.concatStrings (
      lib.mapAttrsToList (
        path: file: linkInEtc (lib.removePrefix "/etc/" path) (filled file)
      ) etcTemplates
    )}
    ${lib.concatStrings (
      lib.mapAttrsToList (name: home: ''
        mkdir -p $out/profiles/per-user
        ln -s ${
          pkgs.buildEnv {
            name = "user-${name}";
            paths = map package home.packages;
            ignoreCollisions = true;
          }
        } $out/profiles/per-user/${name}
      '') userProfiles
    )}
    mkdir -p $out/distro
    ln -s ${pkgs.writeText "model.json" input.model} $out/distro/model.json
    ln -s ${pkgs.writeText "model-hash" input.model_hash} $out/distro/model-hash
    mkdir -p $out/nix
    ln -s ${pkgs.writeText "nix.conf" (
      lib.concatLines (sum "nixSettings" ++ [ "!include settings.conf" ])
    )} $out/nix/nix.conf
  '';

  initrd = import ./initrd.nix {
    inherit lib pkgs moduleTree writeUnits firmware;
    kernel = linux;
    modprobeConfig = kernel.modprobe;
    hostname = base.system.hostname;
    units = union "initrd.units";
    fstab = sum "initrd.fstab";
    kernelModules = sum "initrd.kernelModules" ++ kernel.initrd_modules;
    storePaths = sum "initrd.storePaths";
    disks = lib.any (layer: layer.initrd.disks or false) allLayers;
    crypttab = sum "initrd.crypttab";
    udevRules = lib.concatMapStrings (layer: layer.initrd.udevRules or "") allLayers;
    dlopenFeatures = sum "initrd.dlopenFeatures";
    ssh = union "initrd.ssh";
  };

  kernelParams =
    base.boot.kernel_params
    ++ sum "kernelParams"
    ++ lib.optional (kernel.firmware != [ ]) "firmware_class.path=${firmware}/lib/firmware";

  # The CPU reads microcode from an archive at the very start of the initrd.
  bootInitrd =
    if kernel ? microcode then
      pkgs.runCommand "initrd-with-microcode" { } ''
        cat ${package kernel.microcode.package}/${kernel.microcode.image} ${initrd}/initrd > $out
      ''
    else
      "${initrd}/initrd";

  toplevel = pkgs.runCommand "distro-system" { } ''
    mkdir $out
    ln -s ${linux}/bzImage $out/kernel
    ln -s ${moduleTree} $out/kernel-modules
    ln -s ${bootInitrd} $out/initrd
    ln -s ${etc} $out/etc
    ln -s ${activation} $out/activation.json
    ln -s ${sw} $out/sw
    ln -s ${systemd} $out/systemd
    # The paths modules name, as distro's own systems have them: programs in bin, libraries and
    # data in lib and share, and each user's own packages in users.
    mkdir -p $out/bin
    for program in ${sw}/bin/*; do ln -s "$program" $out/bin/; done
    ln -s ${sw}/lib $out/lib
    ln -s ${sw}/share $out/share
    if [ -d ${etc}/profiles/per-user ]; then ln -s ${etc}/profiles/per-user $out/users; fi
    echo ${lib.escapeShellArgs factoryInputs} > $out/factory-inputs
    echo -n ${lib.escapeShellArg (lib.concatLines (sum "initrdSecrets"))} > $out/initrd-secrets
    echo "init=$out/init ${lib.concatStringsSep " " kernelParams}" > $out/kernel-params
    # The booted system's first program: distro puts the system in place and starts systemd.
    cat > $out/init <<EOF
    #!${pkgs.runtimeShell}
    exec ${distro}/bin/distro activate --mode init $out
    EOF
    chmod +x $out/init
    # How distro switches to this system: `switch`, `test` or `boot`.
    cat > $out/bin/switch-to-configuration <<EOF
    #!${pkgs.runtimeShell}
    exec ${distro}/bin/distro activate --mode "\$1" $out
    EOF
    chmod +x $out/bin/switch-to-configuration
  '';
in
toplevel
