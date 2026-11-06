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
      # where its packages come from as a lock records it. Packages come from the lock's source,
      # or this flake's nixpkgs without one. When the model extends an existing NixOS
      # configuration, the shim is layered onto it and its packages are that configuration's.
      evalInput =
        input: extra:
        let
          json = lib.importJSON input;
          settings = json.nixos;
          modules = [
            ./shim.nix
            { _module.args.distroInput = input; }
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
          packageSet = if json.source ? url then pinned json.source else nixpkgs;
        in
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
              url = json.source.url or defaultUrl;
              rev = packageSet.rev or null;
              hash = packageSet.narHash;
            };
          };

      # The system to build. A lock pins the configuration a system extends too, so one that
      # changed since is refused until it's locked again.
      fromInput =
        input: extra:
        let
          evaluated = evalInput input extra;
          locked = (lib.importJSON input).source;
        in
        if locked ? hash && locked.hash != evaluated.source.hash then
          throw (
            "${evaluated.source.url} changed since distro.lock was written; "
            + "run `distro lock` to use it as it is now"
          )
        else
          evaluated.system;

      # What the lock records: the source and, for each package the system and its users have,
      # its version and store path.
      resolve =
        input:
        let
          evaluated = evalInput input [ ];
          json = lib.importJSON input;
          names = lib.unique (
            json.packages ++ lib.concatMap (home: home.packages) (builtins.attrValues json.homes)
          );
          locked =
            name:
            let
              package = lib.getAttrFromPath (lib.splitString "." name) evaluated.system.pkgs;
            in
            {
              version = package.version or (builtins.parseDrvName package.name).version;
              id = package.outPath;
            };
        in
        {
          inherit (evaluated) source;
          packages = lib.genAttrs names locked;
        };
    in
    {
      packages.${system} = {
        inherit agent distro;
        harness-fixture = image "harness-fixture" (nixos [ ./harness-fixture.nix ]);
      };

      # Entry points for `distro`, which passes the path of the shim's input.
      lib.${system} = {
        inherit resolve;
        toplevel = input: (fromInput input [ ]).config.system.build.toplevel;
        testToplevel = input: (fromInput input diskHarness).config.system.build.toplevel;
        testImage = input: image "distro-test" (fromInput input diskHarness);
        testVmToplevel = input: (fromInput input hostStoreHarness).config.system.build.toplevel;
        testVm = input: hostStoreVm "distro-test-vm" (fromInput input hostStoreHarness);
      };
    };
}
