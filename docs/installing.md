# Installing

There are two ways onto a machine right now: boot a USB stick with wrath on it and install onto
the machine's disks, or run it in a VM. Both start the same way, with a configuration describing
the machine, its disks included.

Fair warning: installing onto disks is tested in VMs, encrypted disks, TPM unlock and all, but I
haven't done it on real hardware yet. My desktop runs wrath from a USB stick for now. Expect
rough edges, and don't point it at a disk with anything on it you care about.

## Describe the disks

Disks go in the configuration like everything else:

```toml
# One encrypted disk: a boot partition and a btrfs filesystem on the rest.
[disks.main]
partitions.esp = { size = "512M", format = "vfat", mount = "/boot" }
partitions.system = { size = "rest", encrypt = true }

[filesystems.root]
on = ["main.system"]
subvolumes."@root" = { mount = "/" }
subvolumes."@wrath" = { mount = "/wrath" }
subvolumes."@home" = { mount = "/home" }
```

The disk is described by what it should hold, not by its device path, so the same description
works on machines with different hardware. `wrath install` matches the machine's disks to it,
and refuses to guess when more than one would fit. Two disks with btrfs RAID 1 across them works
too:

```toml
[disks.nvme]
devices = { count = 2, kind = "nvme", at_least = "500G" }
```

`encrypt = true` means LUKS, with a passphrase you type at boot. Add this to have the TPM unlock
it instead:

```toml
[unlock]
tpm2 = true
```

Of note: the TPM unlock only checks the Secure Boot state right now (PCR 7), and wrath doesn't
do Secure Boot yet. So anyone with the machine in their hands can boot something else and get the
key. Fine for a desktop at home, not fine for a laptop you travel with. A passphrase doesn't have
that problem.

## From a USB stick

Build an image of the machine's configuration, with the configuration inside it. Any Linux
machine that can run wrath's builder can do this (see [Getting started](getting-started.md) for
what it needs). The first build compiles everything from source and takes roughly an hour on a
fast machine:

```sh
./target/release/wrath -C ~/framework build --image --with-configuration -o framework.img
```

Write it to a USB stick (this erases the stick, so double check the device):

```sh
sudo dd if=framework.img of=/dev/sdX bs=4M status=progress conv=fsync
```

Boot the machine from it and log in. The stick runs the system you described, without its disks,
which aren't there yet, and carries the system with them, built and ready to put on the disks:

```sh
sudo wrath install -C /etc/wrath --disk /dev/nvme0n1
```

That lays out the disks with systemd-repart, asks for a passphrase for the encrypted parts,
copies the system over, sets up the boot loader and makes it generation 1. It shows what it's
about to do and asks first. A disk that already has partitions is refused unless you add
`--wipe`.

Install copies the configuration directory to `/etc/wrath` on the new disks. That's the copy the
installed machine applies from, so after the first boot you edit it there (or copy your own
checkout over it) and run `wrath apply`, which builds what's new on the machine itself.

Grapes isn't published yet, so a stick built from a checkout leaves the lock without a pinned
grapes, and the first apply on the machine has nowhere to fetch it from. Every system carries the
grapes it was built from, so give it that one until you pin your own:

```sh
wrath apply --source grapes=/run/system/grapes
```

`--name` records the machine's name on it. It's the hostname when `system.hostname` isn't set,
and it's how a configuration shared by several machines tells them apart. `--role` picks which
of the configuration's `roles/` this machine is, for a configuration with roles, and is recorded
too, so later commands on the machine default to it.

### Secrets from the first boot

A machine decrypts its secrets with its SSH host key, and a fresh install makes a new one, so
secrets encrypted to the old key don't decrypt until you add the new one and re-encrypt them.
Make the host key before you build the image instead, and every install uses that one:

```sh
wrath secret machine framework -C ~/machines/framework
```

That keeps the key in `secrets/machines/framework/`, encrypted to `system.secret_recipients` and
to a passphrase it asks for, and prints its public half to add to `secret_recipients`. Install
finds the key by `--name` or the hostname, asks for the passphrase, and puts the key in the new
system, so its secrets decrypt at the first boot and SSH doesn't warn about a changed host key
after a reinstall. `--identity FILE`, with a key it's encrypted to, works instead of the
passphrase, and `--key-passphrase-file FILE` gives the passphrase without asking.

### Getting online

A wired connection works out of the box, with DHCP. For wifi, turn it on in the configuration
before you build the image, along with the firmware most laptop wifi cards need:

```toml
[networking.wireless]
enable = true
country = "SE"

[hardware]
firmware = ["linux-firmware"]
```

That runs iwd, on the stick and on the installed system. Join a network with:

```sh
iwctl station wlan0 connect "My Network"
```

iwd remembers networks in `/var/lib/iwd`, and the installed system starts with its own empty
one, so join once more after the first boot. From then on it joins by itself (list
`/var/lib/iwd` under `[state]` if `/` is erased at every boot). `iwctl station list` shows the
interface's name if it isn't `wlan0`.

## In a VM

[Getting started](getting-started.md) has the details. In short:

```sh
./target/release/wrath -C ~/my-machine build --image -o vm.img
qemu-system-x86_64 -enable-kvm -m 2G -bios "$OVMF" \
  -drive file=vm.img,format=raw,if=virtio -snapshot -nographic
```

## Managing it from somewhere else

Once a machine runs wrath you can change it from wherever you have the configuration:

```sh
wrath plan --host root@framework
wrath apply --host root@framework
```

That builds the new system locally, copies over what the machine doesn't have yet over SSH, and
switches it with the new system's own wrath. `NAME` is whatever `ssh NAME` reaches, so your
`~/.ssh/config` applies. The login needs root, or sudo, which asks for its password once.

## What's missing

- A proper live installer image you can download. Today you build your own.
- Installing a system other than the one on the stick, which needs wrath's builder and grapes
  on the stick too.
- Secure Boot, and with it a TPM unlock that's actually hard to get around.
