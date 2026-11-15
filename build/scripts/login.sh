#!/usr/bin/env bash
# Boots an image the way the Framework Desktop would see a USB stick (see vm.sh). Logs in on the
# serial port as a user with a password from the configuration, writes to their home, becomes
# root through sudo, checks that wrath on it sees it runs its configuration, and saves a picture
# of the screen.
set -euo pipefail

configuration=${CONFIGURATION:-scenarios/fixtures/native-password}
user=${LOGIN_USER:-demo}
password=${LOGIN_PASSWORD:-wrath}
work=work/login
image=$work/usb.img
log=$work/login.log
mkdir -p "$work"
source scripts/vm.sh

# Serial last, so the initrd and systemd talk on the port this script reads.
scripts/image.sh "$configuration" "$image" "console=tty0 console=ttyS0,115200"
vm_start

result=failed
if wait_for "login: " 180; then
    send "$user"
    wait_for "Password: " 20 && send "$password"
    if wait_for "\\$ " 30; then
        send 'echo "LOGIN as $(id -un) home $HOME shell $0"'
        send 'echo written > ~/probe && echo "HOME-WRITE $(cat ~/probe)"'
        send 'echo "MOUNTS $(findmnt -no SOURCE,OPTIONS /home)"'
        send "printf '%s\\n' '$password' | sudo -S -p '' id -un | sed 's/^/SUDO-AS /'"
        send 'wrath --source grapes=/run/system/grapes status 2>&1 | grep -q "it matches the configuration" && echo STATUS-MATCHES'
        send 'echo "SSHD $(systemctl is-active sshd 2>&1)"'
        if wait_for "HOME-WRITE written" 20 && wait_for "MOUNTS " 10 \
            && wait_for "SUDO-AS root" 20 && wait_for "STATUS-MATCHES" 30; then
            result=passed
        fi
    fi
fi
screendump "$work/screen.png"
vm_stop
plain_log \
    | grep -aE -e 'INITRD|login:|LOGIN|HOME-WRITE|MOUNTS|SUDO-AS' \
        -e 'sudo:|wrappers:|Login incorrect|FAILED|STATUS-|SSHD|wrath:' \
    | cut -c1-200
echo "login $result"
[[ $result == passed ]]
