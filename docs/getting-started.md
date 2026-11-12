# Getting started

The quickest way to see distro is to describe a small machine, build a disk image of it and boot
that in a VM. Nothing on your own machine changes, and it takes a few minutes plus however long
the first build runs.

## What you need

A Linux machine on x86_64 with KVM, and these on it:

- Rust, to build distro itself
- bubblewrap, curl, git, tar and xz, which the builder uses to fetch and build in a sandbox
- dosfstools, mtools, e2fsprogs and util-linux, to make the disk image
- QEMU and OVMF (UEFI firmware for QEMU), to boot it

If you use [devenv](https://devenv.sh), `devenv shell` in the repository has all of it, and sets
`$OVMF` to the firmware.

Then build distro from a checkout:

```sh
git clone https://github.com/johnae/distro
cd distro
cargo build --release
```

There are no releases yet, so commands below run `./target/release/distro` from the checkout.

## Describe a machine

A configuration is just a directory with a `system.toml` in it. Make one somewhere, like so:

```toml
# ~/my-machine/system.toml
packages = ["ripgrep"]

[system]
hostname = "playground"

[users.me]
extra_groups = ["wheel"]
# The password hash for logging in. Make your own with `openssl passwd -6`.
initial_hashed_password = '$6$...'
```

`distro check` reads it and tells you if anything's wrong, without building anything. It also
writes `distro.lock` next to it. That normally pins the distro revision and with it every
package's version, but a distro built from a checkout is pinned by the checkout itself, so for
now the lock just keeps your user's uid:

```console
$ ./target/release/distro -C ~/my-machine check
config /home/me/my-machine
no errors: 106 options, 27 packages, distro unpinned, since it was built from a checkout, model f4df682c49f67cb5
```

27 packages for one in the file? Those are what any system needs (systemd, a shell, sudo and so
on), plus ripgrep.

Typo an option and you get the line and a suggestion:

```console
$ ./target/release/distro -C ~/my-machine check
error: unknown option `system.hostnme`
 --> /home/me/my-machine/system.toml:4:1
  |
4 | hostnme = "playground"
  | ^^^^^^^ no option with this name
  = help: did you mean `hostname`?
```

## Build an image

```sh
./target/release/distro -C ~/my-machine build --image -o playground.img
```

The first time, this builds everything from source: the compiler, the C library, systemd, the
kernel and the rest. That's roughly an hour on a fast machine. After that only what changed gets
built, and a small change takes seconds. `-v` shows everything the builds print, if you want to
watch.

`playground.img` is a link to the image in distro's store, so don't edit it in place. The
`-snapshot` below keeps QEMU from writing to it.

## Boot it

```sh
qemu-system-x86_64 -enable-kvm -m 2G -smp 2 -bios "$OVMF" \
  -drive file=playground.img,format=raw,if=virtio -snapshot -nographic
```

`$OVMF` is the firmware file, something like `/usr/share/edk2/x64/OVMF.fd` or
`/usr/share/OVMF/OVMF.fd` depending on your distribution. After a few seconds you get a login
prompt on the console:

```text
playground login: me
Password:
$ rg --version
ripgrep 14.1.1
$ distro status
```

`Ctrl-a x` quits QEMU.

## Change something

Add a package and a service to `system.toml`:

```toml
packages = ["ripgrep", "jq"]

[services.ssh]
enable = true
```

`distro plan` shows what that changes, compared with an empty machine here since this one isn't
running distro:

```sh
./target/release/distro -C ~/my-machine plan --fresh
```

Build the image again and boot it, and the new one has both. Only jq, OpenSSH and the bits that
tie the system together get built this time.

On a real machine you don't rebuild images, of course. You run `distro apply`, which shows the
plan, asks, and switches the running system. That's what [Setting up a machine](setting-up.md)
is about (not written yet).

## What next

- [How it works](how-it-works.md), for what just happened under the hood.
- `distro options` lists every option there is, and `distro options services.ssh` the ones under
  it.
