# Installing

There are two ways onto a machine right now: boot a USB stick with distro on it and install onto
the machine's disks, or run it in a VM. Both start the same way, with a configuration describing
the machine, its disks included.

Fair warning: installing onto disks is tested in VMs, encrypted disks, TPM unlock and all, but I
haven't done it on real hardware yet. My desktop runs distro from a USB stick for now. Expect
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
subvolumes."@store" = { mount = "/store" }
subvolumes."@home" = { mount = "/home" }
```

The disk is described by what it should hold, not by its device path, so the same description
works on machines with different hardware. `distro install` matches the machine's disks to it,
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

Of note: the TPM unlock only checks the Secure Boot state right now (PCR 7), and distro doesn't
do Secure Boot yet. So anyone with the machine in their hands can boot something else and get the
key. Fine for a desktop at home, not fine for a laptop you travel with. A passphrase doesn't have
that problem.

## From a USB stick

Build an image of the machine's configuration, with the configuration inside it:

```sh
./target/release/distro -C ~/framework build --image --with-configuration -o framework.img
```

Write it to a USB stick (this erases the stick, so double check the device):

```sh
sudo dd if=framework.img of=/dev/sdX bs=4M status=progress conv=fsync
```

Boot the machine from it and log in. The stick runs the exact system you described, so the
quickest install is to put that same system onto the disks:

```sh
sudo distro install -C /etc/distro --disk /dev/nvme0n1 --system "$(readlink /run/system)"
```

That lays out the disks with systemd-repart, asks for a passphrase for the encrypted parts,
copies the system over, sets up the boot loader and makes it generation 1. It shows what it's
about to do and asks first. A disk that already has partitions is refused unless you add
`--wipe`.

For a machine with a role, `--role` and `--name` record which kind of machine it is and what
it's called. If you keep SSH host keys for it encrypted in the configuration
(`secrets/machines/NAME/`), `--identity FILE` decrypts them into the new system, so its host key
is known before it ever boots.

## In a VM

[Getting started](getting-started.md) has the details. In short:

```sh
./target/release/distro -C ~/my-machine build --image -o vm.img
qemu-system-x86_64 -enable-kvm -m 2G -bios "$OVMF" \
  -drive file=vm.img,format=raw,if=virtio -snapshot -nographic
```

## Managing it from somewhere else

Once a machine runs distro you can change it from wherever you have the configuration:

```sh
distro plan --host root@framework
distro apply --host root@framework
```

That builds the new system locally, copies over what the machine doesn't have yet over SSH, and
switches it with the new system's own distro. `NAME` is whatever `ssh NAME` reaches, so your
`~/.ssh/config` applies. The login needs root, or sudo, which asks for its password once.

The machine keeps a copy of the configuration it boots in `/etc/distro`, replaced by each apply,
so you can see there what it runs. Planning and building need distro's recipes, which the
machine's own distro doesn't have, so they happen where your checkout is.

## What's missing

- A proper live installer image you can download. Today you build your own.
- Installing a system other than the one on the stick, which needs distro's builder and recipes
  on the stick too.
- Secure Boot, and with it a TPM unlock that's actually hard to get around.
