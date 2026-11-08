# 0013: A root erased at every boot

Status: accepted, 2026-11-08.

## Context

neptune, cygnus and icarus run with `/` in memory and a list of paths to keep. It shows at a
glance what a machine holds that isn't in its configuration, which fits being auditable, and a
machine can't drift. An appliance or server wants the same thing.

## Decision

`state.path` names a mount to keep things in. With it set, `/` is a tmpfs, and the mount joins the
root and the store in the initrd. `[state]` lists `directories` and `files`, and a user's file
lists its own, relative to their home.

The activator does the keeping, at every boot and switch:

- A kept directory or an existing file is bind-mounted from under the state path.
- A file that doesn't exist yet is a link to where it will be kept, so whatever makes it writes it
  there. It's mounted from the next boot.
- `/etc/distro` and `/var/lib/distro` are always kept.
- Users and groups are saved under `/var/lib/distro`, so a user made at run time keeps its id and
  a changed password stays.

Something that can't be kept is reported, and the machine boots anyway.

## Alternatives

Resetting `/` from a blank btrfs snapshot at each boot. It needs no memory for `/` and survives
large writes, but only works on btrfs, and needs its own step in the initrd. A tmpfs is what the
machines run today.

## Consequences

- A secret's identity has to be kept, or the secret can't be decrypted after a reboot. Evaluating
  the configuration says so.
- Dropping a path stops keeping it. What was kept stays under the state path until someone removes
  it.
- Anything else written to `/` is gone at the next boot, including what a program expected to
  find there. The list has to be complete, and finding out what's missing takes a reboot.
