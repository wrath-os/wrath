{ pkgs, config, ... }:

{
  languages.rust.enable = true;

  packages = [
    pkgs.age
    pkgs.cargo-insta
    pkgs.qemu_kvm
    # The VM harness: forwarding each of a guest's connections to a cache on the host.
    pkgs.socat
    pkgs.swtpm
    pkgs.taplo
    # The builder: its sandbox, fetching sources, and making disk images.
    pkgs.bubblewrap
    pkgs.composefs
    pkgs.cpio
    pkgs.curl
    pkgs.dosfstools
    pkgs.e2fsprogs
    pkgs.erofs-utils
    pkgs.gnutar
    pkgs.mtools
    pkgs.xz
  ];

  # UEFI firmware for the VM harness.
  env.OVMF_CODE = "${pkgs.OVMF.fd}/FV/OVMF_CODE.fd";
  env.OVMF_VARS = "${pkgs.OVMF.fd}/FV/OVMF_VARS.fd";
  env.OVMF = "${pkgs.OVMF.fd}/FV/OVMF.fd";

  # A checkout of grapes beside this one, for the tests and building while working on both.
  env.WRATH_GRAPES = "${config.devenv.root}/../grapes";

  enterTest = ''
    set -euo pipefail
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo run --quiet --package wrath-cli -- dev test fixtures/bad-configs
    cargo run --quiet --package wrath-cli -- dev test "$WRATH_GRAPES/modules/tests"
    cargo run --quiet --package wrath-cli -- dev docs --check
  '';
}
