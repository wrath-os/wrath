#!/usr/bin/env bash
# A bootable disk image of one distro configuration: a GPT with an EFI system partition holding
# systemd-boot, the kernel and the initrd, and an ext4 root with the store and one generation.
# Write it to a USB stick to boot a PC from it.
#
#   scripts/image.sh <configuration, from the repository root> <image> [kernel options]
#
# Runs from `build/`, with distro and lab built in release mode.
#
# The kernel options default to the screen as the console, with the serial port as well. A
# failure in the initrd opens a shell there.
set -euo pipefail

[[ $# -ge 2 ]] || { sed -n '6p' "$0" >&2; exit 2; }
configuration=$1
image=$2
consoles=${3:-console=ttyS0,115200 console=tty0}
arm=${ARM:-f}
lab=../target/release/lab
distro=$PWD/../target/release/distro
work=work/image
root=$work/root
label=distro-root

if [[ -d $root ]]; then
    chmod -R u+w -- "$root"
    rm -r -- "$root"
fi
mkdir -p "$root/store" "$root/system/generations"

(cd .. && "$distro" -C "$configuration" dev model 2>/dev/null) > "$work/model.json"
generation=$(cd .. && "$distro" -C "$configuration" build --target generation)
generation=${generation##*/}
echo "generation $generation"

declare -A store
while read -r kind one two; do
    case $kind in
        root) store[$one]=$two ;;
        path) cp -a --reflink=auto "work/store/$one" "$root/store/" ;;
    esac
done < <("$lab" graph-closure "$arm" "$work/model.json" | grep -E '^(root|path) ')
cp -a "work/store/$generation" "$root/store/"
ln -s "/store/$generation" "$root/system/generations/1"
ln -s generations/1 "$root/system/current"
mkdir -p "$root"/{bin,etc,dev,proc,sys,run,tmp,var/lib,root,home,srv}
ln -s "/store/${store[bash]}/bin/bash" "$root/bin/sh"
[[ ! -e $root/usr ]] || { echo "the image has a /usr" >&2; exit 1; }

esp=$work/esp.img
rm -f -- "$esp"
truncate -s 256M "$esp"
mkfs.vfat -F 32 -n ESP "$esp" > /dev/null
mmd -i "$esp" ::/EFI ::/EFI/BOOT ::/EFI/distro ::/loader ::/loader/entries
mcopy -i "$esp" "work/store/${store[systemd]}/lib/systemd/boot/efi/systemd-bootx64.efi" \
    ::/EFI/BOOT/BOOTX64.EFI
files=()
for name in kernel initrd; do
    path=$(readlink "work/store/$generation/$name")
    directory=${path#/store/}
    directory=${directory%%/*}
    [[ -e $root/store/$directory ]] || cp -a --reflink=auto "work/store/$directory" "$root/store/"
    mcopy -i "$esp" "work/store/$directory/${path##*/}" "::/EFI/distro/$directory-$name"
    files+=("/EFI/distro/$directory-$name")
done
{
    echo "title distro"
    echo "linux ${files[0]}"
    echo "initrd ${files[1]}"
    echo "options $(cat "work/store/$generation/kernel-params") distro.rescue $consoles" \
        "init=/store/$generation/init"
} > "$work/entry.conf"
mcopy -i "$esp" "$work/entry.conf" ::/loader/entries/distro.conf
printf 'default distro.conf\ntimeout 3\n' > "$work/loader.conf"
mcopy -i "$esp" "$work/loader.conf" ::/loader/loader.conf

# Room for logs and homes on top of what's there.
megabytes=$(( $(du -sm "$root" | cut -f1) + 2048 ))
system=$work/root.img
rm -f -- "$system" "$image"
truncate -s "${megabytes}M" "$system"
bwrap --unshare-user --uid 0 --gid 0 --bind / / --dev-bind /dev /dev -- \
    mke2fs -q -t ext4 -L "$label" -d "$root" "$system"
truncate -s "$((megabytes + 258))M" "$image"
sfdisk --quiet "$image" <<TABLE
label: gpt
start=2048, size=524288, type=uefi
start=526336, size=$((megabytes * 2048)), type=linux
TABLE
dd if="$esp" of="$image" bs=1M seek=1 conv=notrunc status=none
dd if="$system" of="$image" bs=1M seek=257 conv=notrunc,sparse status=none
echo "image $image $(du -h --apparent-size "$image" | cut -f1)"
