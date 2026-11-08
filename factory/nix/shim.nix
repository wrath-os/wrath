# Maps the lowered model onto NixOS options. This is fixed code: everything that differs between
# systems comes from `distroInput`, the JSON that `distro` writes, so no Nix is generated.
#
# The model's own files, units, packages and users are added as they are. Settings of the base
# system are defaults, so on a system that extends an existing NixOS configuration, that
# configuration wins where the two overlap, with a warning. Moving a setting to TOML means
# deleting it in Nix.
{
  config,
  lib,
  pkgs,
  distro,
  factoryInputs,
  distroInput,
  distroSources,
  ...
}:

let
  input = lib.importJSON distroInput;
  base = input.base;
  session = base.session or null;
  storage = base.storage or {
    partitions = [ ];
    mounts = [ ];
    spanning = [ ];
    unlock = {
      tpm2 = false;
      ssh.authorized_keys = [ ];
    };
  };
  inherit (storage) unlock;
  # Filesystems across disks that boot with a member missing, and the members that may be.
  degraded = lib.filter (fs: fs.degraded == "boot") storage.spanning;
  mapperName = lib.removePrefix "/dev/mapper/";
  optional = lib.concatMap (fs: map mapperName (lib.tail fs.members)) degraded;
  replaces = input.nixos.replaces or [ ];
  package = import ./package.nix lib pkgs distroSources;
  inherit (lib) mkDefault;

  # Where the machine fills in a file with secrets in it.
  filled = file: "/run/distro/templates/${file.name}";
  etcTemplates = lib.filterAttrs (path: _: lib.hasPrefix "/etc/" path) input.templates;
  otherTemplates = lib.filterAttrs (path: _: !(lib.hasPrefix "/etc/" path)) input.templates;
  # Every file with secrets in it, the system's and users'.
  templates =
    lib.attrValues input.templates
    ++ lib.concatMap (home: lib.attrValues (home.templates or { })) (lib.attrValues input.homes);

  # Base settings NixOS overrides, as the TOML option, its value, the NixOS option and the value
  # NixOS uses.
  overridden = lib.filter (check: check.ours != check.theirs) [
    {
      option = "system.hostname";
      ours = base.system.hostname;
      nixos = "networking.hostName";
      theirs = config.networking.hostName;
    }
    {
      option = "system.timezone";
      ours = base.system.timezone;
      nixos = "time.timeZone";
      theirs = config.time.timeZone;
    }
    {
      option = "networking.dhcp";
      ours = base.networking.dhcp;
      nixos = "networking.useDHCP";
      theirs = config.networking.useDHCP;
    }
    {
      option = "boot.loader";
      ours = base.boot.loader;
      nixos = "boot.loader";
      theirs = if config.boot.loader.systemd-boot.enable then "systemd-boot" else "another loader";
    }
  ];
in
{
  system.stateVersion = mkDefault "26.05";

  warnings =
    map (
      check:
      "the TOML's ${check.option} = ${builtins.toJSON check.ours} is ignored: NixOS's ${check.nixos} is ${builtins.toJSON check.theirs}"
    ) overridden
    ++ lib.optional (
      config.networking.firewall.enable && input.units ? "nftables.service"
    ) "the NixOS firewall runs alongside the TOML's networking.firewall; turn one of them off"
    ++ map (name: "nixos.replaces names ${name}, which no module renders") (
      lib.filter (name: !(input.units ? ${name})) replaces
    );

  environment.systemPackages = [ distro ] ++ map package input.packages;
  # distro builds on the machine from the factory it's installed with; keeping the factory's
  # inputs lets it evaluate without the network.
  system.extraDependencies = factoryInputs;

  environment.etc =
    lib.mapAttrs (_: file: { inherit (file) text mode; }) input.etc
    // lib.mapAttrs' (
      path: file: lib.nameValuePair (lib.removePrefix "/etc/" path) { source = filled file; }
    ) etcTemplates
    // {
      "distro/model.json".text = input.model;
      "distro/model-hash".text = input.model_hash;
    };

  # Files outside /etc are links into the store, or to where files with secrets are filled in.
  systemd.tmpfiles.rules =
    lib.mapAttrsToList (
      path: file: "L+ ${path} - - - - ${pkgs.writeText (baseNameOf path) file.text}"
    ) input.files
    ++ lib.mapAttrsToList (path: file: "L+ ${path} - - - - ${filled file}") otherTemplates;

  # A unit NixOS also defines is a conflict, unless the TOML says it replaces that one.
  systemd.units = lib.mapAttrs (name: unit: {
    text = if lib.elem name replaces then lib.mkForce unit.text else unit.text;
    wantedBy = unit.wanted_by;
    requiredBy = unit.required_by;
  }) input.units;

  # Services NixOS provides that reload or restart when the model's files they read change.
  systemd.services = lib.mapAttrs (_: trigger: {
    reloadTriggers = lib.optional (trigger ? reload) trigger.reload;
    restartTriggers = lib.optional (trigger ? restart) trigger.restart;
  }) input.triggers;

  # Secrets are decrypted on every boot and switch into a new directory on ramfs, which then
  # replaces /run/secrets in one step. One that fails to decrypt is left out with an error.
  system.activationScripts.distroSecrets = lib.mkIf (input.secrets != { }) {
    deps = [
      "specialfs"
      "users"
      "groups"
      "etc"
    ];
    text =
      let
        identities = lib.escapeShellArgs base.secrets.identities;
        decrypt =
          name: secret:
          let
            source = builtins.path {
              path = /. + secret.source;
              name = "${name}.age";
            };
          in
          lib.escapeShellArgs [
            "decrypt"
            name
            source
            secret.owner
            secret.mode
            secret.link
          ];
      in
      ''
        (
        umask 0077
        mkdir -p /run/secrets.d
        grep -q " /run/secrets.d ramfs " /proc/mounts ||
          mount -t ramfs none /run/secrets.d -o nodev,nosuid,mode=0751
        generation=/run/secrets.d/$(date +%s%N)
        mkdir -m 0751 "$generation"
        identities=()
        for identity in ${identities}; do
          [ -e "$identity" ] && identities+=(-i "$identity")
        done
        decrypt() {
          local name=$1 source=$2 owner=$3 mode=$4 link=$5 file="$generation/$1"
          if ${pkgs.age}/bin/age -d "''${identities[@]}" -o "$file" "$source"; then
            chown "$owner" "$file"
            chmod "$mode" "$file"
          else
            echo "cannot decrypt secret $name" >&2
          fi
          if [ -n "$link" ]; then
            (umask 0022 && mkdir -p "$(dirname "$link")")
            ln -sfn "/run/secrets/$name" "$link"
          fi
        }
        ${lib.concatStringsSep "\n" (lib.mapAttrsToList decrypt input.secrets)}
        ln -sfn "$generation" /run/secrets.next
        mv -T /run/secrets.next /run/secrets
        for old in /run/secrets.d/*; do
          [ "$old" = "$generation" ] || rm -rf "$old"
        done
        )
      '';
  };

  # Files with secrets or machine values in them are filled in on every boot and switch, after
  # the secrets, into a new directory on ramfs that then replaces /run/distro/templates in one
  # step. A file that can't be filled in is left out with an error. Their paths link into that
  # directory.
  system.activationScripts.distroTemplates = lib.mkIf (templates != [ ]) {
    deps = [
      "specialfs"
      "users"
      "groups"
    ]
    ++ lib.optional (input.secrets != { }) "distroSecrets";
    text =
      let
        spec = pkgs.writeText "distro-templates.json" (
          builtins.toJSON (
            lib.listToAttrs (map (file: lib.nameValuePair file.name file.parts) templates)
          )
        );
        own =
          file:
          lib.escapeShellArgs [
            "own"
            file.name
            file.owner
            file.mode
          ];
      in
      ''
        (
        umask 0077
        mkdir -p /run/distro/templates.d
        grep -q " /run/distro/templates.d ramfs " /proc/mounts ||
          mount -t ramfs none /run/distro/templates.d -o nodev,nosuid,mode=0751
        generation=/run/distro/templates.d/$(date +%s%N)
        mkdir -m 0751 "$generation"
        ${distro}/bin/distro fill-templates ${spec} /run/secrets "$generation" || true
        own() {
          [ -e "$generation/$1" ] || return 0
          chown "$2" "$generation/$1"
          chmod "$3" "$generation/$1"
        }
        ${lib.concatStringsSep "\n" (map own templates)}
        ln -sfn "$generation" /run/distro/templates.next
        mv -T /run/distro/templates.next /run/distro/templates
        for old in /run/distro/templates.d/*; do
          [ "$old" = "$generation" ] || rm -rf "$old"
        done
        )
      '';
  };

  # A configuration without a hostname leaves NixOS's empty and has the machine's name filled
  # into /etc/hostname with the other files: systemd reads it on boot, and a switch sets it here.
  system.activationScripts.distroHostname = lib.mkIf (base.system.hostname == "") {
    deps = [
      "etc"
      "distroTemplates"
    ];
    text = ''
      if [ -s /etc/hostname ]; then
        tr -d '\n' < /etc/hostname > /proc/sys/kernel/hostname
      fi
    '';
  };

  # Users' files are links into the store, made on every boot and switch.
  system.activationScripts.distroHomes = {
    deps = [
      "users"
      "groups"
    ];
    text =
      let
        link =
          name: user:
          let
            home = input.homes.${name} or { };
            links = import ./home-links.nix { inherit lib pkgs; } name home filled;
          in
          "PATH=${config.systemd.package}/bin:$PATH "
          + lib.escapeShellArgs [
            "${pkgs.bash}/bin/bash"
            "${./link-home.sh}"
            name
            user.group
            user.home
            links
          ];
      in
      lib.concatStringsSep "\n" (
        lib.mapAttrsToList link (lib.filterAttrs (_: user: !user.system) base.users)
      );
  };

  # Login shells pick up the user's session variables; systemd reads environment.d itself.
  environment.extraInit = ''
    if [ -r "$HOME/.config/distro/session.sh" ]; then
      . "$HOME/.config/distro/session.sh"
    fi
  '';

  # The disks `distro install` laid out, unlocked and mounted by the names it gave them.
  boot.initrd.luks.devices = lib.listToAttrs (
    map (
      partition:
      lib.nameValuePair partition.label (
        {
          device = "/dev/disk/by-partlabel/${partition.label}";
          crypttabExtraOpts =
            lib.optional (lib.elem partition.label optional) "nofail"
            ++ lib.optional unlock.tpm2 "tpm2-device=auto";
        }
      )
    ) (lib.filter (partition: partition.encrypt) storage.partitions)
  );
  fileSystems = lib.listToAttrs (
    map (
      mount:
      lib.nameValuePair mount.path (
        {
          inherit (mount) device;
          fsType = mount.format;
        }
        // lib.optionalAttrs (mount.options != [ ]) { inherit (mount) options; }
      )
    ) (lib.filter (mount: mount.format != "swap") storage.mounts)
  );
  swapDevices = map (mount: { inherit (mount) device; }) (
    lib.filter (mount: mount.format == "swap") storage.mounts
  );
  # Unlocking over SSH in the initrd, with the key install placed for it, for machines without
  # someone at the keyboard.
  boot.initrd.network = lib.mkIf (unlock.ssh.authorized_keys != [ ]) {
    enable = true;
    ssh = {
      enable = true;
      inherit (unlock.ssh) port;
      authorizedKeys = unlock.ssh.authorized_keys;
      hostKeys = [ "/etc/ssh/initrd_ed25519_key" ];
    };
  };

  # btrfs holds back a filesystem's devices until every member is there, so one with a member
  # missing would never mount. Instead the initrd waits for the members a while, then mounts with
  # what's there, and leaves a note for `distro status`.
  boot.initrd.services.udev.rules = lib.concatMapStrings (
    fs: lib.concatMapStrings (member: ''
      ENV{DM_NAME}=="${mapperName member}", ENV{SYSTEMD_READY}="1"
    '') fs.members
  ) degraded;
  boot.initrd.systemd.services = lib.listToAttrs (
    map (
      fs:
      lib.nameValuePair "distro-wait-${fs.name}" {
        description = "Wait for the disks of ${fs.name}";
        after = [ "cryptsetup.target" ];
        before = [ "sysroot.mount" ];
        requiredBy = [ "sysroot.mount" ];
        unitConfig.DefaultDependencies = false;
        serviceConfig.Type = "oneshot";
        script = ''
          for _ in $(seq 30); do
            missing=""
            for member in ${lib.escapeShellArgs fs.members}; do
              [ -e "$member" ] || missing="$missing $member"
            done
            [ -z "$missing" ] && exit 0
            sleep 1
          done
          message="distro: ${fs.name} boots degraded, without$missing"
          echo "$message"
          echo "$message" > /dev/console
          mkdir -p /run/distro
          echo "$missing" > /run/distro/degraded-${fs.name}
        '';
      }
    ) degraded
  );

  networking.hostName = mkDefault base.system.hostname;
  time.timeZone = mkDefault base.system.timezone;

  # distro builds with Nix on the machine itself, which needs these.
  nix.settings.experimental-features = [
    "nix-command"
    "flakes"
  ];

  users.mutableUsers = mkDefault false;
  users.users = lib.mapAttrs (
    name: user:
    {
      inherit (user)
        description
        group
        home
        ;
      extraGroups = user.extra_groups;
      packages = map package (input.homes.${name}.packages or [ ]);
      inherit (user) linger;
      isSystemUser = user.system;
      isNormalUser = !user.system;
      createHome = !user.system;
    }
    // lib.optionalAttrs (user ? uid) { inherit (user) uid; }
    // lib.optionalAttrs (user ? shell) { inherit (user) shell; }
  ) base.users;
  # A system user's group has the user's name unless it says otherwise.
  users.groups = lib.mapAttrs (_: _: { }) (
    lib.filterAttrs (name: user: user.system && user.group == name) base.users
  );

  # The firewall module renders its own ruleset.
  networking.firewall.enable = mkDefault false;

  boot.loader.systemd-boot.enable = mkDefault (base.boot.loader == "systemd-boot");
  boot.kernelParams = base.boot.kernel_params;

  networking.useNetworkd = mkDefault true;
  networking.useDHCP = mkDefault base.networking.dhcp;
  # Only ever turned on: networkd enables resolved by default too.
  services.resolved.enable = lib.mkIf base.networking.resolved true;
  # Nothing else asks for the network to be up, but the system is expected to reach it.
  systemd.targets.network-online.wantedBy = [ "multi-user.target" ];

  services.pipewire = lib.mkIf base.audio.enable {
    enable = true;
    alsa.enable = true;
    pulse.enable = true;
  };
  security.rtkit.enable = lib.mkIf base.audio.enable true;
  # PipeWire is socket activated; start it with the user's session so audio is ready.
  systemd.user.services.pipewire.wantedBy = lib.mkIf base.audio.enable [ "default.target" ];

  programs.niri.enable = lib.mkIf (session != null && session.name == "niri") true;
  services.greetd = lib.mkIf (session != null) {
    enable = true;
    settings =
      {
        default_session.command = mkDefault "${pkgs.tuigreet}/bin/tuigreet --cmd niri-session";
      }
      // lib.optionalAttrs (session ? autologin) {
        initial_session = {
          user = session.autologin;
          command = "niri-session";
        };
      };
  };
}
