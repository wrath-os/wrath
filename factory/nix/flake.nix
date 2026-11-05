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
      agent = pkgs.callPackage ./agent.nix { };
      harnessFixture = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = { inherit agent; };
        modules = [ ./harness-fixture.nix ];
      };
    in
    {
      packages.${system} = {
        inherit agent;

        # Test images follow one contract: the output directory contains `disk.qcow2`, a UEFI
        # bootable disk whose system runs the guest agent on its default vsock port.
        harness-fixture = pkgs.runCommand "harness-fixture" { } ''
          mkdir $out
          ln -s ${harnessFixture.config.system.build.images.qemu-efi}/*.qcow2 $out/disk.qcow2
        '';
      };
    };
}
