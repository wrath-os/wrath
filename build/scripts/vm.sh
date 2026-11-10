# Sourced by the boot tests. Boots $image the way the Framework Desktop would see a USB stick:
# UEFI, the image on USB storage, an empty NVMe disk beside it, no virtio. The serial port's
# output goes to $log, and `send` types on it.
#
# Needs $work, $image and $log set.

vm_start() {
    local nvme=$work/nvme.img
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
}

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

# Saves a picture of the screen to $1.
screendump() {
    echo "screendump $PWD/$1 -f png" >&4
    sleep 1
}

vm_stop() {
    kill "$qemu" 2> /dev/null || true
    wait "$qemu" 2> /dev/null || true
}
