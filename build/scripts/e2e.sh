#!/usr/bin/env bash
# From a distro configuration to a running system: lowers two configurations that differ by one
# line into generations, boots each under UEFI, and switches from the first to the second live.
# Runs from `build/`, with distro and lab built in release mode.
set -euo pipefail

arm=${ARM:-f}
lab=../target/release/lab
distro=$PWD/../target/release/distro
work=work/e2e/boot
root=$work/root
log=work/e2e/boot.log
measures=work/e2e/measures.txt

model() { (cd .. && "$distro" -C "fixtures/configs/$1" dev model 2>/dev/null); }
graph() { "$lab" graph "$arm" "$1" | grep -E '^(generation|actions|ms) '; }
# Lowers a model and records the apply, with what the configuration in $2 keeps.
apply() {
    (cd .. && "$distro" -C "fixtures/configs/$2" dev apply-record 2>/dev/null) \
        > "$1.record"
    "$lab" graph-apply "$arm" "$1" "$1.record" | grep -E '^(generation|actions|ms|apply|skipped) '
}

if [[ -d $root ]]; then
    chmod -R u+w -- "$root"
    rm -r -- "$root"
fi
mkdir -p "$root/store" "$root/system/generations" work/e2e

{
    echo "--- evaluate first configuration"
    time model native-web > work/e2e/one.json
    echo "--- first lowering, nothing known"
    apply work/e2e/one.json native-web | tee work/e2e/one.out
    echo "--- same configuration again"
    graph work/e2e/one.json
    echo "--- the one-line change, evaluate and lower"
    time {
        model native-web-changed > work/e2e/two.json
        apply work/e2e/two.json native-web-changed | tee work/e2e/two.out
    }
} 2>&1 | tee "$measures"

first=$(awk '$1=="generation"{print $2}' work/e2e/one.out)
second=$(awk '$1=="generation"{print $2}' work/e2e/two.out)
{
    echo "--- what changes"
    "$lab" graph-diff "$(awk '$1=="apply"{print $2}' work/e2e/one.out)" \
        "$(awk '$1=="apply"{print $2}' work/e2e/two.out)"
} | tee -a "$measures"

declare -A store
while read -r kind one two; do
    case $kind in
        root) store[$one]=$two ;;
        path) cp -a --reflink=auto "work/store/$one" "$root/store/" ;;
    esac
done < <("$lab" graph-closure "$arm" work/e2e/two.json | grep -E '^(root|path) ')

number=1
for generation in "$first" "$second"; do
    cp -a "work/store/$generation" "$root/store/"
    ln -s "/store/$generation" "$root/system/generations/$number"
    number=$((number + 1))
done
ln -s "/store/$first" "$root/system/current"
mkdir -p "$root"/{bin,etc,dev,proc,sys,run,tmp,var/lib,root,home,srv}
ln -s "/store/${store[bash]}/bin/bash" "$root/bin/sh"
[[ ! -e $root/usr ]] || { echo "the image has a /usr" >&2; exit 1; }

# Each generation's kernel and initrd are in the store. The boot partition holds copies.
for generation in "$first" "$second"; do
    for name in kernel initrd; do
        path=$(readlink "work/store/$generation/boot/$name")
        directory=${path#/store/}
        directory=${directory%%/*}
        [[ -e $root/store/$directory ]] \
            || cp -a --reflink=auto "work/store/$directory" "$root/store/"
    done
done
[[ -z $(grep -rla '/nix/store' "$root/store" | head -1) ]] && echo "no /nix/store references" \
    | tee -a "$measures"

options="root=LABEL=root rw console=ttyS0 panic=-1 systemd.getty_auto=0"
disk=$work/disk.img
esp=$work/esp.img
system=$work/root.img
rm -f -- "$disk" "$esp" "$system"
truncate -s 256M "$esp"
mkfs.vfat -F 32 -n ESP "$esp" > /dev/null
mmd -i "$esp" ::/EFI ::/EFI/BOOT ::/EFI/distro ::/loader ::/loader/entries
mcopy -i "$esp" "work/store/${store[systemd]}/lib/systemd/boot/efi/systemd-bootx64.efi" \
    ::/EFI/BOOT/BOOTX64.EFI
number=1
declare -A copied
for generation in "$first" "$second"; do
    files=()
    for name in kernel initrd; do
        path=$(readlink "work/store/$generation/boot/$name")
        directory=${path#/store/}
        directory=${directory%%/*}
        esp_name=$directory-$name
        if [[ -z ${copied[$esp_name]:-} ]]; then
            mcopy -i "$esp" "work/store/$directory/${path##*/}" "::/EFI/distro/$esp_name"
            copied[$esp_name]=1
        fi
        files+=("/EFI/distro/$esp_name")
    done
    entry=$work/gen-$number.conf
    {
        echo "title Generation $number"
        echo "linux ${files[0]}"
        echo "initrd ${files[1]}"
        echo "options $options generation=$generation"
    } > "$entry"
    mcopy -i "$esp" "$entry" "::/loader/entries/gen-$number.conf"
    number=$((number + 1))
done

truncate -s 5500M "$system"
bwrap --unshare-user --uid 0 --gid 0 --bind / / --dev-bind /dev /dev -- \
    mke2fs -q -t ext4 -L root -d "$root" "$system"
truncate -s 6G "$disk"
sfdisk --quiet "$disk" <<TABLE
label: gpt
start=2048, size=524288, type=uefi
start=526336, size=11264000, type=linux
TABLE
dd if="$esp" of="$disk" bs=1M seek=1 conv=notrunc status=none
dd if="$system" of="$disk" bs=1M seek=257 conv=notrunc,sparse status=none

pick() {
    printf 'default gen-%s.conf\ntimeout 0\n' "$1" > "$work/loader.conf"
    mcopy -o -i "$disk@@1M" "$work/loader.conf" ::/loader/loader.conf
}
: > "$log"
for entry in 1 2 1; do
    echo "=== boot entry $entry" | tee -a "$log"
    pick "$entry"
    timeout "${BOOT_TIMEOUT:-120}" qemu-system-x86_64 -enable-kvm -m 2G -smp 4 -nographic \
        -no-reboot -bios "$OVMF" -drive "file=$disk,if=virtio,format=raw" \
        -nic user,model=virtio-net-pci \
        | tee -a "$log" || true
done
plain=$(sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g; s/\r$//' "$log")
grep -aE 'E2E |=== boot|INITRD' <<< "$plain" | sed -E 's/^.*(E2E |INITRD)/\1/' | cut -c1-200

# Passes when each boot reached a running system with no failed units, and the switch happened.
count() { grep -acE "$1" <<< "$plain" || true; }
if [[ $(count 'E2E DONE') == 3 && $(count 'E2E state=running') == 3 \
    && $(count 'E2E failed: ?$') == 3 && $(count 'E2E switched') == 1 ]]; then
    echo "e2e passed"
else
    echo "e2e failed" >&2
    exit 1
fi
