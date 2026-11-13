#!/usr/bin/env bash
# A bootable disk image of one wrath configuration, to write to a USB stick or boot in a VM.
#
#   scripts/image.sh <configuration, from the repository root> <image> [kernel options]
#
# Runs from `build/`, with wrath built in release mode. The kernel options default to the screen
# as the console, with the serial port as well. A failure in the initrd opens a shell there.
set -euo pipefail

[[ $# -ge 2 ]] || { sed -n '4p' "$0" >&2; exit 2; }
options=()
for option in wrath.rescue ${3:-console=ttyS0,115200 console=tty0}; do
    options+=(--kernel-option "$option")
done
built=$(cd .. && target/release/wrath -C "$1" build --image --with-configuration \
    "${options[@]}")
mv -f -- "$built" "$2"
echo "image $2 $(du -h --apparent-size "$2" | cut -f1)"
