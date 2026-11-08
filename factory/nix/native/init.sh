#!@bash@/bin/bash
# The first program on the booted system: puts the system in place, then hands over to systemd.
# The initrd's /dev, /proc, /sys and /run came along when it switched to this root.
set -euo pipefail
umask 022

system=@out@
export PATH=@path@

mkdir -p /etc /var /tmp /root /home /bin /usr/bin
chmod 1777 /tmp
chmod 700 /root

ln -sfn "$system" /run/current-system
ln -sfn "$system" /run/booted-system

# /etc links each file to the system's through /etc/static, so switching moves one link. Accounts
# are copied, since systemd-sysusers adds the system's own users to them.
ln -sfn "$system/etc" /etc/static
(
  cd "$system/etc"
  find . ! -type d -printf '%P\n'
) | while read -r path; do
  case "$path" in
    passwd | group) install -m 0644 "$system/etc/$path" "/etc/$path" ;;
    shadow) install -m 0600 "$system/etc/$path" "/etc/$path" ;;
    *)
      mkdir -p "$(dirname "/etc/$path")"
      ln -sfn "/etc/static/$path" "/etc/$path"
      ;;
  esac
done

ln -sfn "$system/sw/bin/bash" /bin/sh
ln -sfn "$system/sw/bin/env" /usr/bin/env

exec @systemd@/lib/systemd/systemd "$@"
