# Installing

There are two ways onto a machine right now: boot a USB stick with wrath on it and install onto
the machine's disks, or run it in a VM. Both start the same way, with a configuration describing
the machine, its disks included.

Fair warning: installing onto disks is tested in VMs, encrypted disks, TPM unlock and all, and
on one real machine so far, my Framework desktop. Expect rough edges, and don't point it at a
disk with anything on it you care about.

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

Of note: without Secure Boot (below), the TPM unlock only checks the Secure Boot state (PCR 7),
so anyone with the machine in their hands can boot something else and get the key. Fine for a
desktop at home, not fine for a laptop you travel with. A passphrase doesn't have that problem.

A laptop can ask for a PIN along with the TPM, so the key can't be had from the TPM alone, even
from a stolen machine. It's shorter than a passphrase, and the TPM locks out guessing:

```toml
[unlock]
tpm2 = true
pin = true
```

`wrath install` asks for the PIN, and `sudo wrath unlock enroll` changes it.

### Recovery keys

Install also makes a recovery key for each encrypted disk. It unlocks the disk wherever the
passphrase is asked for, for when the passphrase is forgotten or the TPM stops unlocking. It's
shown once, and kept on the boot partition encrypted to `system.secret_recipients`, except the
machine's own key, so the machine can't read it but you can, from any machine with your key:

```sh
sudo mount /dev/sdb1 /mnt            # the locked machine's boot partition, or its /boot
wrath unlock recovery-key --from /mnt
```

`sudo wrath unlock recovery` makes new ones, replacing the old, and is how a machine installed
before this gets them. To keep them somewhere else, say so:

```toml
[unlock.recovery]
keep = "print"                        # shown once, kept nowhere; or "none" for no recovery key
command = ["store-recovery-keys"]     # or a program that takes them instead
```

A program in `command` gets `NAME KEY` lines on its standard input and the machine's name as its
argument, so a password manager's command line, or a module that brings one, can file them away.

### Secure Boot

A machine can boot only what it signed itself, with keys of its own. Every switch signs the boot
loader and each generation's image, and the TPM hands over the disks' key only to the initrd of an
image the machine signed. Each image carries a policy signed with the machine's key, so a new
generation needs no new enrollment, and nothing booted any other way gets the key.

Before turning it on, check that nothing in the machine needs Microsoft's keys. A graphics or
network card with firmware of its own needs them, and shows nothing once the firmware trusts only
the machine's keys. On the machine:

```sh
sudo /run/system/systemd/lib/systemd/systemd-pcrlock log | grep -i driver
```

Lines naming `efi-variable-driver-config` are the Secure Boot settings themselves and are fine.
Anything else, such as a driver loaded from a card, means that card needs Microsoft's keys; leave
Secure Boot off on that machine for now. Laptops and machines with integrated graphics usually
show nothing more.

Then, for a machine called `framework`:

1. Make its keys, along with its host key if it doesn't have one yet:

   ```sh
   wrath secret machine framework
   ```

   They're kept encrypted in `secrets/machines/framework/`: what the machine signs with, for the
   machine and `system.secret_recipients`, and the keys that change what its firmware trusts, for
   every recipient but the machine.

2. Turn it on in the machine's configuration:

   ```toml
   [boot]
   secure_boot = true
   ```

3. On the machine, switch to it:

   ```sh
   sudo wrath apply
   ```

4. Restart into the firmware's setup screen and put Secure Boot in setup mode. It's usually
   under Security or Secure Boot. Deleting only the platform key (PK) does it and keeps the list
   of revoked boot loaders; "Erase all Secure Boot settings" works too. Leave "Restore factory
   keys" alone, which puts Microsoft's back. Some firmware, such as the Framework's, also needs
   "Enforce Secure Boot" turned on. Save and restart.

5. Pick "Enroll Secure Boot keys" in the boot menu. It warns that this can leave the machine
   unable to boot, which the check above rules out. The machine restarts with Secure Boot on and
   asks for the disks' passphrase once, since the TPM sees a different boot.

6. Once it's up, have the TPM unlock the disks again, now only for images the machine signed, and
   check that Secure Boot is on:

   ```sh
   sudo wrath unlock enroll
   bootctl status | grep 'Secure Boot'
   ```

   It says `enabled (user)`.

If the machine won't boot, turn Secure Boot off in the firmware's setup screen. It boots the same
images unsigned, and the passphrase still unlocks the disks.

The images also attach only the disks whose volume keys the machine recorded. A disk swapped in
with the same labels and UUIDs, which the TPM would hand the key to all the same, is refused, even
given the passphrase. Install records the keys; for a machine installed before, the
`wrath unlock enroll` in step 6 does.

The TPM only unlocks the disks while the boot looks the same to it. A firmware update, or a
machine's first boot from a unified kernel image (wrath says so when it switches), changes that,
and the next boot asks for the passphrase. Once it's up, have the TPM unlock them again:

```sh
sudo wrath unlock enroll
```

### Unlocking over SSH

A server in a rack, or any machine you're not sitting at, can ask for its passphrase over SSH
instead. List the keys that may log in:

```toml
[unlock.ssh]
authorized_keys = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHVXpgKZYrIfk13p8f+bhA/aS4Nmuwmf3RaZssxOmr/3 me"]
```

At boot the initrd brings up the wired network with DHCP and listens on port 2222
(`unlock.ssh.port`). Log in as root and answer the prompt:

```sh
ssh -p 2222 root@server systemd-tty-ask-password-agent --query
```

The initrd presents a key of its own, the machine's initrd key, so you can tell it's really your
machine before you type anything. `wrath secret machine` makes it ahead along with the host key
and prints it, or install makes one and prints it. It's kept sealed to the machine's TPM next to
the boot loader, so a copy of the ESP doesn't give it away.

## From a configuration repository

The simplest way: one installer stick for every machine, with each machine's configuration in a
repository and what it runs in a cache. Build the installer once, on any Linux machine that can
run wrath's builder (see [Getting started](getting-started.md)):

```sh
wrath build --image -C github:wrath-os/wrath//installer -o installer.img
sudo dd if=installer.img of=/dev/sdX bs=4M status=progress conv=fsync
```

Boot the machine from it. It logs root in on the screen. Join a network if it isn't wired
(`iwctl station wlan0 connect "My Network"`), then install what the repository describes for it:

```sh
wrath install -C github:you/machines//framework --disk /dev/nvme0n1
```

The configuration is fetched, and the system with it from the caches it names (`[caches]`, see
[How it works](how-it-works.md)), so the stick carries none of it. The machine records the
repository as where it came from, and upgrades from there, with `[updates]`, from its first
boot. A private repository asks for a GitHub token that can read it. The installer can't
build what the caches don't have, so build the configuration where the cache is filled, and push
it, first.

To install over SSH instead, copy `installer/` from wrath's repository, add your key under
`[services.ssh]`, and build that.

## From an image of the machine

An image of one machine's configuration carries the configuration and the system, built,
which suits a machine with no network, or no cache to fetch from. The first build compiles
everything from source and takes roughly an hour on a fast machine:

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

`--name` records the machine's name on it. It's the hostname when `system.hostname` isn't set,
and it's how a configuration shared by several machines tells them apart. `--role` picks which
of the configuration's `roles/` this machine is, for a configuration with roles, and is recorded
too, so later commands on the machine default to it.

## Secrets from the first boot

A machine decrypts its secrets with its SSH host key, and a fresh install makes a new one, so
secrets encrypted to the old key don't decrypt until you add the new one and re-encrypt them.
Make the host key before you build the image instead, and every install uses that one:

```sh
wrath secret machine framework -C ~/machines/framework
```

That keeps the key in `secrets/machines/framework/`, with the initrd's key for unlocking over
SSH, encrypted to `system.secret_recipients` and to a passphrase it asks for, and prints their
public halves: add the host key to `secret_recipients`. Install finds the keys by `--name` or the
hostname, asks for the passphrase, and puts them in the new system, so its secrets decrypt at the
first boot and SSH doesn't warn about a changed host key after a reinstall. `--identity FILE`, with a key it's encrypted to, works instead of the
passphrase, and `--key-passphrase-file FILE` gives the passphrase without asking.

## Getting online

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

- An installer image you can download. Today you build it, once.
- Installing onto the disks directly: what install fetches goes into the stick's store first and
  is copied from there, so the stick needs room for the whole system.
