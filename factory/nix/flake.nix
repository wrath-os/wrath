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

      nixos =
        modules:
        nixpkgs.lib.nixosSystem {
          inherit system;
          specialArgs = { inherit agent distro; };
          modules = modules ++ [ { nixpkgs.pkgs = pkgs; } ];
        };

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

      # A system from the shim's input, the JSON `distro` writes, plus extra NixOS modules. When
      # the model extends an existing NixOS configuration, the shim is layered onto it.
      fromInput =
        input: extra:
        let
          settings = (nixpkgs.lib.importJSON input).nixos;
          modules = [
            ./shim.nix
            { _module.args.distroInput = input; }
          ]
          ++ map (path: /. + path) (settings.modules or [ ])
          ++ extra;
          # Plain paths are read through git, like this flake, so build outputs are left out.
          flakeRef =
            if nixpkgs.lib.hasPrefix "/" settings.flake then
              "git+file://${settings.flake}"
            else
              settings.flake;
        in
        if settings ? flake then
          (builtins.getFlake flakeRef).nixosConfigurations.${settings.host}.extendModules {
            inherit modules;
            specialArgs = { inherit agent distro; };
          }
        else
          nixos modules;
    in
    {
      packages.${system} = {
        inherit agent distro;
        harness-fixture = image "harness-fixture" (nixos [ ./harness-fixture.nix ]);
      };

      # Entry points for `distro`, which passes the path of the shim's input.
      lib.${system} = {
        toplevel = input: (fromInput input [ ]).config.system.build.toplevel;
        testToplevel = input: (fromInput input diskHarness).config.system.build.toplevel;
        testImage = input: image "distro-test" (fromInput input diskHarness);
        testVmToplevel = input: (fromInput input hostStoreHarness).config.system.build.toplevel;
        testVm = input: hostStoreVm "distro-test-vm" (fromInput input hostStoreHarness);
      };
    };
}
