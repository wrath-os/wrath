{
  description = "Build infrastructure for distro. This directory is the only place Nix is used.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
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
      distro = pkgs.callPackage ./rust-package.nix {
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

      inherit (nixpkgs) lib;

      # A NixOS system with packages from `packageSet`, a nixpkgs flake.
      nixosFrom =
        packageSet: modules:
        packageSet.lib.nixosSystem {
          inherit system;
          specialArgs = { inherit agent distro; };
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
      ];
      hostStoreHarness = [
        ./test-harness.nix
        ./test-host-store.nix
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
          locked = name: flake: {
            url = sources.${name}.url;
            rev = flake.rev or null;
            hash = flake.narHash;
          };
          default =
            if settings ? flake then
              {
                system = extended.nixosConfigurations.${settings.host}.extendModules {
                  inherit modules;
                  specialArgs = { inherit agent distro; };
                };
                source = {
                  url = flakeRef;
                  rev = extended.rev or null;
                  hash = extended.narHash;
                };
              }
            else
              {
                system = nixosFrom packageSet modules;
                source = {
                  url = sources.nixpkgs.url or defaultUrl;
                  rev = packageSet.rev or null;
                  hash = packageSet.narHash;
                };
              };
        in
        {
          inherit (default) system;
          inherit named;
          sources = lib.mapAttrs locked named // {
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
    in
    {
      packages.${system} = {
        inherit agent distro;
        harness-fixture = image "harness-fixture" (nixos [ ./harness-fixture.nix ]);
      };

      # Entry points for `distro`, which passes the path of the shim's input.
      lib.${system} = {
        inherit environment resolve;
        toplevel = input: (fromInput input [ ]).config.system.build.toplevel;
        testToplevel = input: (fromInput input diskHarness).config.system.build.toplevel;
        testImage = input: image "distro-test" (fromInput input diskHarness);
        testVmToplevel = input: (fromInput input hostStoreHarness).config.system.build.toplevel;
        testVm = input: hostStoreVm "distro-test-vm" (fromInput input hostStoreHarness);
      };
    };
}
