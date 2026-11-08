{
  description = "Build infrastructure for distro. This directory is the only place Nix is used.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        config = { };
        overlays = [ ];
      };
      agent = pkgs.callPackage ./rust-package.nix {
        pname = "distro-agent";
        crates = [ "agent" ];
        mainProgram = "distro-agent";
      };
      cli = pkgs.callPackage ./rust-package.nix {
        pname = "distro-cli";
        crates = [
          "agent"
          "backend-nixos"
          "check"
          "cli"
          "eval"
          "model"
          "starlark"
        ];
        mainProgram = "distro";
      };
      # distro as installed on a machine: the binary, and under `share/distro` what it builds from,
      # this factory, the modules and the crates the factory compiles, so the machine can evaluate
      # and build its own configuration. Built from a committed revision, `share/distro/source.toml`
      # says which, so a lock can pin it and an older checkout builds with it again.
      distro =
        let
          root = ../..;
          source = lib.fileset.toSource {
            inherit root;
            fileset = lib.fileset.unions [
              (root + "/Cargo.toml")
              (root + "/Cargo.lock")
              (root + "/crates")
              (root + "/factory/nix")
              (root + "/modules")
            ];
          };
          identity = pkgs.writeText "source.toml" ''
            url = "${upstream}"
            rev = "${self.rev or ""}"
            hash = "${self.narHash}"
          '';
        in
        pkgs.runCommand "distro" { meta.mainProgram = "distro"; } (
          ''
          mkdir -p $out/bin $out/share
          cp ${cli}/bin/distro $out/bin/distro
          cp -r ${source} $out/share/distro
        ''
        + lib.optionalString (self ? rev) ''
          chmod u+w $out/share/distro
          cp ${identity} $out/share/distro/source.toml
        ''
        );

      inherit (nixpkgs) lib;
      # Where distro lives, which a lock pinning a distro fetches it from. Empty until it has a home;
      # a configuration can say with `[sources.distro]`.
      upstream = "";
      # The factory's pinned inputs, which machines keep so they can evaluate offline.
      factoryInputs = [ nixpkgs.outPath ];

      # A NixOS system with packages from `packageSet`, a nixpkgs flake.
      nixosFrom =
        packageSet: modules:
        packageSet.lib.nixosSystem {
          inherit system;
          specialArgs = { inherit agent distro factoryInputs; };
          modules = modules ++ [
            {
              nixpkgs.pkgs = import packageSet {
                inherit system;
                config = { };
                overlays = [ ];
              };
            }
          ];
        };
      nixos = nixosFrom nixpkgs;

      # Where packages come from without a lock: this flake's nixpkgs, which follows this URL.
      defaultUrl = "github:NixOS/nixpkgs/nixos-unstable";

      # When a flake's revision was made, as `2026-10-30`, if it says.
      date =
        flake:
        let
          stamp = flake.lastModifiedDate or null;
        in
        if stamp == null then
          null
        else
          "${builtins.substring 0 4 stamp}-${builtins.substring 4 2 stamp}-${builtins.substring 6 2 stamp}";

      # A lock's source as a tree of files, pinned by its hash, or the latest without one. Only
      # sources packages come from are read as flakes, so a source of files needs no flake.nix.
      tree =
        source:
        builtins.fetchTree (
          if source ? hash then
            removeAttrs (builtins.parseFlakeRef source.url) [ "ref" ]
            // { narHash = source.hash; }
            // lib.optionalAttrs (source ? rev) { inherit (source) rev; }
          else
            builtins.parseFlakeRef source.url
        );

      # The nixpkgs a lock's source names, pinned by its hash, or the latest without one.
      pinned =
        source:
        if source ? hash then
          builtins.getFlake (
            builtins.flakeRefToString (
              removeAttrs (builtins.parseFlakeRef source.url) [ "ref" ]
              // { narHash = source.hash; }
              // lib.optionalAttrs (source ? rev) { inherit (source) rev; }
            )
          )
        else
          builtins.getFlake source.url;

      # Test images follow one contract: the output directory contains `disk.qcow2`, a UEFI
      # bootable disk whose system runs the guest agent on its default vsock port.
      image =
        name: system:
        pkgs.runCommand name { } ''
          mkdir $out
          ln -s ${system.config.system.build.image}/*.qcow2 $out/disk.qcow2
        '';

      # Test VMs that boot from the host's store: the output directory contains `kernel`,
      # `initrd` and `kernel-params`, the command line that boots the system. The guest
      # registers the closure in its database from a list it reads through the mounted store.
      hostStoreVm =
        name: system:
        let
          toplevel = system.config.system.build.toplevel;
          registration = pkgs.closureInfo { rootPaths = [ toplevel ]; };
        in
        pkgs.runCommand name { } ''
          mkdir $out
          ln -s ${toplevel}/kernel $out/kernel
          ln -s ${toplevel}/initrd $out/initrd
          echo "$(cat ${toplevel}/kernel-params) init=${toplevel}/init regInfo=/nix/.ro-store/${baseNameOf registration}/registration" > $out/kernel-params
        '';

      diskHarness = [
        ./test-harness.nix
        ./test-disk.nix
        (import ./test-share.nix 10)
      ];
      hostStoreHarness = [
        ./test-harness.nix
        ./test-host-store.nix
        (import ./test-share.nix 10)
      ];
      installedHarness = [
        ./test-harness.nix
        ./test-installed.nix
        (import ./test-share.nix 100)
      ];

      # A system from the shim's input, the JSON `distro` writes, plus extra NixOS modules, and
      # where its packages come from as a lock records them. The base system and packages
      # without a source come from the `nixpkgs` source, or this flake's nixpkgs without one;
      # other sources are flakes the input names. When the model extends an existing NixOS
      # configuration, the shim is layered onto it and `nixpkgs` is that configuration's.
      evalInput =
        input: extra:
        let
          json = lib.importJSON input;
          settings = json.nixos;
          sources = json.sources or { };
          named = lib.mapAttrs (_: pinned) (removeAttrs sources [ "nixpkgs" ]);
          modules = [
            ./shim.nix
            {
              _module.args = {
                distroInput = input;
                distroSources = named;
              };
            }
          ]
          ++ map (path: /. + path) (settings.modules or [ ])
          ++ extra;
          # Plain paths are read through git, like this flake, so build outputs are left out.
          flakeRef =
            if lib.hasPrefix "/" settings.flake then
              "git+file://${settings.flake}"
            else
              settings.flake;
          extended = builtins.getFlake flakeRef;
          packageSet = if sources ? nixpkgs then pinned sources.nixpkgs else nixpkgs;
          locked =
            name: source:
            let
              fetched = tree source;
            in
            {
              inherit (source) url;
              rev = fetched.rev or null;
              hash = fetched.narHash;
              date = date fetched;
            };
          default =
            if settings ? flake then
              {
                system = extended.nixosConfigurations.${settings.host}.extendModules {
                  inherit modules;
                  specialArgs = { inherit agent distro factoryInputs; };
                };
                source = {
                  url = flakeRef;
                  rev = extended.rev or null;
                  hash = extended.narHash;
                  date = date extended;
                };
              }
            else
              {
                system = nixosFrom packageSet modules;
                source = {
                  url = sources.nixpkgs.url or defaultUrl;
                  rev = packageSet.rev or null;
                  hash = packageSet.narHash;
                  date = date packageSet;
                };
              };
        in
        {
          inherit (default) system;
          inherit named;
          sources = lib.mapAttrs locked (removeAttrs sources [ "nixpkgs" ]) // {
            nixpkgs = default.source;
          };
        };

      # The system to build. A lock pins the configuration a system extends too, so one that
      # changed since is refused until it's locked again.
      fromInput =
        input: extra:
        let
          evaluated = evalInput input extra;
          locked = (lib.importJSON input).sources.nixpkgs or { };
          current = evaluated.sources.nixpkgs;
        in
        if locked ? hash && locked.hash != current.hash then
          throw (
            "distro: ${current.url} changed since distro.lock was written; "
            + "run `distro lock` to use it as it is now"
          )
        else
          evaluated.system;

      # What the lock records: each source and, for each package the system and its users have,
      # its version and store path.
      resolve =
        input:
        let
          evaluated = evalInput input [ ];
          json = lib.importJSON input;
          package = import ./package.nix lib evaluated.system.pkgs evaluated.named;
          names = lib.unique (
            json.packages ++ lib.concatMap (home: home.packages) (builtins.attrValues json.homes)
          );
          locked =
            name:
            let
              found = package name;
            in
            {
              version = found.version or (builtins.parseDrvName found.name).version;
              id = found.outPath;
            };
        in
        {
          inherit (evaluated) sources;
          packages = lib.genAttrs names locked;
        };

      # The packages the input names, in one environment for `distro run` and `distro shell`:
      # their programs under `bin`, and `share/distro/packages.json` with each package's version
      # and the program `run` starts.
      environment =
        input:
        let
          evaluated = evalInput input [ ];
          json = lib.importJSON input;
          inherit (evaluated.system) pkgs;
          package = import ./package.nix lib pkgs evaluated.named;
          describe =
            name:
            let
              found = package name;
              parsed = builtins.parseDrvName found.name;
            in
            {
              version = found.version or parsed.version;
              program = found.meta.mainProgram or found.pname or parsed.name;
            };
          described = pkgs.writeTextDir "share/distro/packages.json" (
            builtins.toJSON {
              packages = lib.genAttrs json.packages describe;
              inherit (evaluated) sources;
            }
          );
        in
        pkgs.buildEnv {
          name = "distro-environment";
          paths = map package json.packages ++ [ described ];
        };

      # A user's home on its own, for a configuration of one home: `profile`, their packages in
      # one environment, the model it was built from, and `activate`, which links their files into
      # the home as the user who runs it. Login shells find the packages through
      # `~/.config/distro/profile.sh`, and systemd's user manager through environment.d.
      home =
        input:
        let
          evaluated = evalInput input [ ];
          json = lib.importJSON input;
          inherit (evaluated.system) pkgs;
          package = import ./package.nix lib pkgs evaluated.named;
          name = builtins.head (builtins.attrNames json.homes);
          ours = json.homes.${name};
          bin = "$HOME/.local/state/distro/profiles/home/profile/bin";
          onPath = {
            "~/.config/distro/profile.sh" = {
              text = ''
                export PATH="${bin}:$PATH"
                if [ -r "$HOME/.config/distro/session.sh" ]; then
                  . "$HOME/.config/distro/session.sh"
                fi
              '';
              executable = false;
            };
            "~/.config/environment.d/50-distro-profile.conf" = {
              text = "PATH=${bin}:$PATH\n";
              executable = false;
            };
          };
          templates = lib.attrValues (ours.templates or { });
          links = import ./home-links.nix { inherit lib pkgs; } name (
            ours // { files = ours.files // onPath; }
          ) (file: "~/.local/state/distro/templates/${file.name}");
          # Decrypts the home's secrets and fills in its files with secrets in them, as the user,
          # into new directories under ~/.local/state/distro that replace the old ones at once.
          secrets =
            let
              decrypt =
                name: secret:
                lib.escapeShellArgs [
                  "decrypt"
                  name
                  (builtins.path {
                    path = /. + secret.source;
                    name = "${name}.age";
                  })
                  secret.mode
                  secret.link
                ];
              spec = pkgs.writeText "distro-templates.json" (
                builtins.toJSON (
                  lib.listToAttrs (map (file: lib.nameValuePair file.name file.parts) templates)
                )
              );
              own = file: lib.escapeShellArgs [ "own" file.name file.mode ];
            in
            ''
              umask 0077
              state="$HOME/.local/state/distro"
              mkdir -p "$state/secrets.d" "$state/templates.d"
              generation="$state/secrets.d/$(date +%s%N)"
              mkdir "$generation"
              # Points $state/$1 at the new directory $2 at once and removes the older ones.
              swap() {
                ln -sfn "$2" "$state/$1.next"
                mv -T "$state/$1.next" "$state/$1"
                for old in "$state/$1.d"/*; do
                  [ "$old" = "$2" ] || rm -rf "$old"
                done
              }
              identities=()
              for identity in ${lib.escapeShellArgs (json.base.secrets.identities or [ ])}; do
                identity=''${identity/#\~\//$HOME/}
                [ -e "$identity" ] && identities+=(-i "$identity")
              done
              decrypt() {
                local name=$1 source=$2 mode=$3 link=$4 file="$generation/$1"
                if ${pkgs.age}/bin/age -d "''${identities[@]}" -o "$file" "$source"; then
                  chmod "$mode" "$file"
                else
                  echo "cannot decrypt secret $name" >&2
                fi
                if [ -n "$link" ]; then
                  link=''${link/#\~\//$HOME/}
                  mkdir -p "$(dirname "$link")"
                  ln -sfn "$state/secrets/$name" "$link"
                fi
              }
              ${lib.concatStringsSep "\n" (lib.mapAttrsToList decrypt (json.secrets or { }))}
              swap secrets "$generation"
              filled="$state/templates.d/$(date +%s%N)"
              mkdir "$filled"
              ${distro}/bin/distro fill-templates ${spec} "$state/secrets" "$filled" || true
              own() {
                [ -e "$filled/$1" ] && chmod "$2" "$filled/$1"
              }
              ${lib.concatStringsSep "\n" (map own templates)}
              swap templates "$filled"
            '';
          profile = pkgs.buildEnv {
            name = "distro-home-profile";
            paths = map package ours.packages;
          };
          hasSecrets = (json.secrets or { }) != { } || templates != [ ];
          activate = pkgs.writeShellScript "activate" ''
            ${lib.optionalString hasSecrets secrets}
            PATH=${pkgs.systemd}/bin:$PATH exec ${pkgs.bash}/bin/bash ${./link-home.sh} \
              "$(id -un)" "$(id -gn)" "$HOME" ${links}
          '';
        in
        pkgs.runCommand "distro-home" { } ''
          mkdir -p $out/distro
          ln -s ${profile} $out/profile
          ln -s ${activate} $out/activate
          cp ${pkgs.writeText "model.json" json.model} $out/distro/model.json
          cp ${pkgs.writeText "model-hash" json.model_hash} $out/distro/model-hash
        '';
    in
    {
      packages.${system} = {
        inherit agent distro;
        harness-fixture = image "harness-fixture" (nixos [ ./harness-fixture.nix ]);
      };

      # Entry points for `distro`, which passes the path of the shim's input.
      lib.${system} = {
        inherit environment home resolve;
        toplevel = input: (fromInput input [ ]).config.system.build.toplevel;
        testToplevel = input: (fromInput input diskHarness).config.system.build.toplevel;
        testImage = input: image "distro-test" (fromInput input diskHarness);
        testVmToplevel = input: (fromInput input hostStoreHarness).config.system.build.toplevel;
        testInstalledToplevel =
          input:
          (fromInput input installedHarness).config.system.build.toplevel;
        testVm = input: hostStoreVm "distro-test-vm" (fromInput input hostStoreHarness);
      };
    };
}
