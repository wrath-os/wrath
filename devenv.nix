{ pkgs, ... }:

{
  languages.rust.enable = true;

  packages = [
    pkgs.cargo-insta
    pkgs.qemu_kvm
    pkgs.swtpm
    pkgs.taplo
    pkgs.typescript
  ];

  # UEFI firmware for the VM harness.
  env.OVMF_CODE = "${pkgs.OVMF.fd}/FV/OVMF_CODE.fd";
  env.OVMF_VARS = "${pkgs.OVMF.fd}/FV/OVMF_VARS.fd";

  enterTest = ''
    set -euo pipefail
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    tsc -p modules/typescript
    cargo run --quiet --package distro-cli -- status
  '';
}
