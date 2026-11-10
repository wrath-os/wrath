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
          "backend"
          "build"
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

      # The nixpkgs a lock's source names, pinned by its hash, or the latest without one. The factory's
      # own nixpkgs, when it's the one pinned, is used as it is, without fetching it again.
      pinned =
        source:
        if source.hash or null == nixpkgs.narHash then
          nixpkgs
        else if source ? hash then
          builtins.getFlake (
            builtins.flakeRefToString (
              removeAttrs (builtins.parseFlakeRef source.url) [ "ref" ]
              // { narHash = source.hash; }
              // lib.optionalAttrs (source ? rev) { inherit (source) rev; }
            )
          )
        else
          builtins.getFlake source.url;

      # The input, the JSON `distro` writes, with where its packages come from as a lock records
      # them. Packages without a source come from the `nixpkgs` source, or this flake's nixpkgs
      # without one; other sources are flakes the input names.
      evalInput =
        input:
        let
          json = lib.importJSON input;
          sources = json.sources or { };
          others = removeAttrs sources [ "nixpkgs" ];
          packageSet = if sources ? nixpkgs then pinned sources.nixpkgs else nixpkgs;
          pkgs = import packageSet {
            inherit system;
            config = { };
            overlays = [ ];
          };
          locked =
            _: source:
            let
              fetched = tree source;
            in
            {
              inherit (source) url;
              rev = fetched.rev or null;
              hash = fetched.narHash;
              date = date fetched;
            };
        in
        {
          inherit json pkgs;
          package = import ./package.nix lib pkgs (lib.mapAttrs (_: pinned) others) json.builds;
          sources = lib.mapAttrs locked others // {
            nixpkgs = {
              url = sources.nixpkgs.url or defaultUrl;
              rev = packageSet.rev or null;
              hash = packageSet.narHash;
              date = date packageSet;
            };
          };
        };

      # A system from the input, plus `layers`, each a function of the package set giving parts the
      # model doesn't describe, such as the test harness's agent.
      systemFrom =
        input: layers:
        let
          evaluated = evalInput input;
        in
        import ./native {
          inherit lib distro factoryInputs;
          inherit (evaluated) pkgs package;
          input = evaluated.json;
          layers = map (layer: layer evaluated.pkgs) layers;
        };

      harnessLayer =
        layerPkgs:
        import ./native/test-harness.nix {
          inherit agent;
          pkgs = layerPkgs;
        };
      hostStoreHarness = [
        harnessLayer
        (layerPkgs: import ./native/test-host-store.nix { pkgs = layerPkgs; })
      ];
      # Test VMs that boot from the host's store: the output directory contains `kernel`, `initrd`
      # and `kernel-params`, the command line that boots the system.
      hostStoreVm =
        name: toplevel:
        let
          registration = pkgs.closureInfo { rootPaths = [ toplevel ]; };
        in
        pkgs.runCommand name { } ''
          mkdir $out
          ln -s ${toplevel}/kernel $out/kernel
          ln -s ${toplevel}/initrd $out/initrd
          echo "$(cat ${toplevel}/kernel-params) regInfo=${registration}/registration" \
            > $out/kernel-params
        '';

      # What the lock records: each source and, for each package the system and its users have,
      # its version and store path.
      resolve =
        input:
        let
          evaluated = evalInput input;
          inherit (evaluated) json package;
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
          evaluated = evalInput input;
          inherit (evaluated) json pkgs package;
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
          evaluated = evalInput input;
          inherit (evaluated) json pkgs package;
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
        # A plain NixOS image for testing the harness itself: the output directory contains
        # `disk.qcow2`, a UEFI bootable disk whose system runs the guest agent on its default
        # vsock port. Nothing else here uses NixOS.
        harness-fixture =
          let
            fixture = nixpkgs.lib.nixosSystem {
              inherit system;
              specialArgs = { inherit agent; };
              modules = [
                ./harness-fixture.nix
                { nixpkgs.pkgs = pkgs; }
              ];
            };
          in
          pkgs.runCommand "harness-fixture" { } ''
            mkdir $out
            ln -s ${fixture.config.system.build.image}/*.qcow2 $out/disk.qcow2
          '';
      };

      # Entry points for `distro`, which passes the path of its input.
      lib.${system} = {
        inherit environment home resolve;
        toplevel = input: systemFrom input [ ];
        testVmToplevel = input: systemFrom input hostStoreHarness;
        testInstalledToplevel = input: systemFrom input [ harnessLayer ];
        testVm = input: hostStoreVm "distro-test-vm" (systemFrom input hostStoreHarness);
      };
    };
}
