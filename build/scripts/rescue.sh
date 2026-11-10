#!/usr/bin/env bash
# Boots an image whose boot entry names a root file system that isn't there, and checks that the
# initrd opens a shell on the console instead of powering off, as it would on a PC whose disk it
# can't find.
set -euo pipefail

configuration=${CONFIGURATION:-scenarios/fixtures/native-password}
work=work/rescue
image=$work/usb.img
log=$work/rescue.log
mkdir -p "$work"
source scripts/vm.sh

scripts/image.sh "$configuration" "$image" "console=tty0 console=ttyS0,115200"
# The EFI partition starts at 1 MiB. Point its boot entry at a label no disk has.
entry=$work/entry.conf
mcopy -o -i "$image@@1M" ::/loader/entries/distro.conf "$entry"
sed -i 's/root=LABEL=[^ ]*/root=LABEL=nowhere/' "$entry"
mcopy -o -i "$image@@1M" "$entry" ::/loader/entries/distro.conf
BOOT_TIMEOUT=${BOOT_TIMEOUT:-120} vm_start

result=failed
if wait_for "INITRD rescue shell" 90; then
    send 'echo "RESCUE-OK $((40 + 2))"'
    wait_for "RESCUE-OK 42" 10 && result=passed
fi
vm_stop
plain_log | grep -aE 'INITRD|RESCUE' | cut -c1-200
echo "rescue $result"
[[ $result == passed ]]
