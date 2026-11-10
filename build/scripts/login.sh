#!/usr/bin/env bash
# Boots an image the way the Framework Desktop would see a USB stick: UEFI, the image on USB
# storage, an empty NVMe disk beside it, no virtio. Logs in on the serial port as a user with a
# password from the configuration, writes to their home, becomes root through sudo, and saves a
# picture of the screen.
set -euo pipefail

configuration=${CONFIGURATION:-scenarios/fixtures/native-password}
user=${LOGIN_USER:-demo}
password=${LOGIN_PASSWORD:-distro}
work=work/login
image=$work/usb.img
log=$work/login.log
mkdir -p "$work"

# Serial last, so the initrd and systemd talk on the port this script reads.
scripts/image.sh "$configuration" "$image" "console=tty0 console=ttyS0,115200"
nvme=$work/nvme.img
rm -f -- "$nvme"
truncate -s 1G "$nvme"

for fifo in serial.in serial.out monitor.in monitor.out; do
    rm -f -- "$work/$fifo"
    mkfifo "$work/$fifo"
done
timeout "${BOOT_TIMEOUT:-240}" qemu-system-x86_64 -enable-kvm -m 2G -smp 4 -no-reboot \
    -bios "$OVMF" -display none -vga std -nic none \
    -device qemu-xhci,id=xhci -drive "if=none,id=usb,file=$image,format=raw" \
    -device usb-storage,bus=xhci.0,drive=usb,removable=on -device usb-kbd \
    -drive "if=none,id=nvme,file=$nvme,format=raw" -device nvme,serial=empty,drive=nvme \
    -chardev "pipe,id=serial,path=$work/serial" -serial chardev:serial \
    -chardev "pipe,id=monitor,path=$work/monitor" -mon chardev=monitor &
qemu=$!
exec 3> "$work/serial.in" 4> "$work/monitor.in"
cat "$work/serial.out" > "$log" &
cat "$work/monitor.out" > /dev/null &

plain_log() { sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$log"; }

# Waits until the log shows $1, for at most $2 seconds.
wait_for() {
    for _ in $(seq $(( $2 * 10 ))); do
        plain_log | grep -aq -- "$1" && return 0
        kill -0 "$qemu" 2> /dev/null || return 1
        sleep 0.1
    done
    return 1
}
send() { printf '%s\r' "$1" >&3; }

result=failed
if wait_for "login: " 180; then
    send "$user"
    wait_for "Password: " 20 && send "$password"
    if wait_for "\\$ " 30; then
        send 'echo "LOGIN as $(id -un) home $HOME shell $0"'
        send 'echo written > ~/probe && echo "HOME-WRITE $(cat ~/probe)"'
        send 'echo "MOUNTS $(findmnt -no SOURCE,OPTIONS /home)"'
        send "printf '%s\\n' '$password' | sudo -S -p '' id -un | sed 's/^/SUDO-AS /'"
        if wait_for "HOME-WRITE written" 20 && wait_for "MOUNTS " 10 \
            && wait_for "SUDO-AS root" 20; then
            result=passed
        fi
    fi
fi
echo "screendump $PWD/$work/screen.png -f png" >&4
sleep 1
kill "$qemu" 2> /dev/null || true
wait "$qemu" 2> /dev/null || true
plain_log \
    | grep -aE -e 'INITRD|login:|LOGIN|HOME-WRITE|MOUNTS|SUDO-AS' \
        -e 'sudo:|wrappers:|Login incorrect|FAILED' \
    | cut -c1-200
echo "login $result"
[[ $result == passed ]]
