# 0011: Installing a machine

Status: accepted, 2026-11-07.

## Context

distro switches machines that already run. Putting one on a blank disk still takes NixOS: disko
for partitions, LUKS and btrfs, `nixos-install`, and host keys copied in by hand. The goal is for
distro to do all of it, with nixpkgs only as a source of packages and tools.

The machines this has to cover today: laptops with btrfs on LUKS, desktops and servers with btrfs
RAID 1 across two LUKS disks, initrd SSH to unlock servers remotely, persisted state on its own
subvolume, and SSH host keys made ahead of time and kept encrypted in the configuration.

## Decision

### Disks are described as a tree

A configuration describes its disks: devices, their partitions, encryption, filesystems and what
mounts where. It's explicit, so anyone can read what a machine's disks look like. Common layouts
are files included from a source (0009), so most configurations stay short:

```toml
[disks.nvme]
devices = { count = 2, kind = "nvme", at_least = "500G" }

[disks.nvme.partitions.esp]
size = "1G"
format = "vfat"
mount = "/boot"
on = "first"

[disks.nvme.partitions.system]
size = "rest"
encrypt = true

[filesystems.root]
format = "btrfs"
raid = "raid1"
on = ["nvme.system"]

[filesystems.root.subvolumes]
"@root" = { mount = "/" }
"@nix" = { mount = "/nix", options = ["noatime"] }
"@home" = { mount = "/home" }
"@persist" = { mount = "/persist" }
```

A role describes the disks it wants, not their paths: machines with the same role have different
devices. `distro install` matches the machine's disks to the description, shows the match, and
records it in the machine's `/var/lib/distro/machine.toml` (0008). A configuration for one machine
can name its devices instead, with `paths = ["/dev/disk/by-id/..."]`.

### systemd-repart lays out each disk, distro does what spans disks

systemd-repart partitions each disk from generated definitions, creates the LUKS containers and
formats what's on one disk. A filesystem across disks, such as btrfs RAID 1, is distro's own step
after it: open the containers, create the filesystem on all of them, create subvolumes, and write
what the system needs to unlock and mount them at boot. The tools come from nixpkgs.

repart is systemd's, needs no Nix to lay out a disk, builds images the same way it formats disks,
and can grow a layout later.

### Disks are laid out at install

`install` lays out disks; `apply` never repartitions or reformats. A plan that changes the layout
says it takes a reinstall. Later, `apply` may make changes that destroy nothing, such as a new
subvolume.

### Unlocking

A passphrase is set at install. TPM2 and FIDO2 are enrolled afterwards with `systemd-cryptenroll`,
on every container of the machine. Unlocking over SSH in the initrd is an option for machines
without someone at the keyboard.

A root across several disks needs all of them unlocked before it mounts, in any order. With one
missing, a filesystem's `degraded` setting decides: `"boot"`, the default, waits a while for the
missing disk, then mounts degraded from the others and says so at boot, in the journal and in
`distro status`; `"stop"` waits for every disk, so the machine doesn't come up until someone boots
it by hand. A machine that won't come up is usually worse than one running on fewer disks while
saying so.

### Keys are made ahead of install

A machine's SSH host key, and its initrd key when it unlocks remotely, are made before install and
kept encrypted in the configuration, by convention at
`secrets/machines/<name>/ssh_host_ed25519_key.age` and `.../initrd_ed25519_key.age`. They're
encrypted to the keys of whoever installs.

`distro install --identity FILE` is given an age identity that decrypts them, decrypts them into
the new system, and never copies the identity onto it. The host key is also what decrypts the
machine's own secrets afterwards, as today.

A machine that makes its own key at install, for machines nobody names ahead of time, comes later:
its public key is enrolled and the secrets re-encrypted from another machine.

### Where install runs

`distro install --config DIR|URL --role NAME --identity FILE` runs from any Linux with Nix, such as
the NixOS installer. It lays out the disks, builds the system into them, installs the boot loader
and records the machine's role, name and disks. An installer image of distro's own and installing
another machine over SSH come later.

### Persisted state, later

Keeping only some paths across reboots, with the rest reset, is decided later. Layouts leave room
for it: a `@persist` subvolume can be there from the start.

## Rejected

- disko. It covers more layouts today, but it's a NixOS module run through Nix, the layer distro
  replaces, and it would be replaced later.
- Named layouts as the format (`layout = "btrfs-raid1"`). Short, but what a machine's disks look
  like would be hidden in distro's code. Included files give the same brevity, readable.
- Device paths in roles. Machines with a role don't share device names.
- Machines always making their own keys at install. Every install would need a second step from
  another machine before its secrets work, which is the uncommon case today.

## Consequences

- New modules declare `disks` and `filesystems`; the backend lowers them to repart definitions,
  distro's multi-disk step, and the mounts and unlocking the booted system needs.
- `distro install` joins the commands, and `machine.toml` records the disks a machine got.
- The VM harness gets scenarios that install onto blank disks, with a TPM, and boot what they
  installed.
