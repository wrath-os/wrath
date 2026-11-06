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
  distroInput,
  ...
}:

let
  input = lib.importJSON distroInput;
  base = input.base;
  session = base.session or null;
  replaces = input.nixos.replaces or [ ];
  package = name: lib.getAttrFromPath (lib.splitString "." name) pkgs;
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

  # Files with secrets in them are filled in after the secrets on every boot and switch, into a
  # new directory on ramfs that then replaces /run/distro/templates in one step. A file that
  # can't be filled in is left out with an error. Their paths link into that directory.
  system.activationScripts.distroTemplates = lib.mkIf (templates != [ ]) {
    deps = [ "distroSecrets" ];
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
            files = input.homes.${name}.files or { };
            # A file of the same name in the store, which systemd wants of a link to a unit.
            target =
              path: file:
              let
                base = baseNameOf path;
              in
              "${
                pkgs.writeTextFile {
                  name = lib.strings.sanitizeDerivationName base;
                  destination = "/${base}";
                  inherit (file) text executable;
                }
              }/${base}";
            links = pkgs.writeText "distro-home-${name}" (
              lib.concatStrings (
                lib.mapAttrsToList (path: file: "${path}\t${target path file}\n") files
              )
              + lib.concatStrings (
                lib.mapAttrsToList (path: file: "${path}\t${filled file}\n") (
                  input.homes.${name}.templates or { }
                )
              )
            );
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

  networking.hostName = mkDefault base.system.hostname;
  time.timeZone = mkDefault base.system.timezone;

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
