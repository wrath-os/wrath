# 0029: Uids and gids are kept in the lock

Status: accepted, 2026-11-12.

## Context

Users got uids from 1000 in the order of their names, and extra groups gids from 400 the same
way. Adding `aaron` gave everyone after him a new uid, so their homes and files belonged to
someone else. The `uid`, `group` and `system` options weren't read.

NixOS gives a user without a `uid` the next free one and records it in `/var/lib/nixos/uid-map`,
keeping a removed user's uid so it isn't handed to someone new. That map is state on the
machine: wipe it, or forget to keep it on an erased root, and uids can come out differently.
Userborn and the systemd-sysusers mode read the machine's own `/etc/passwd` instead, which is
still state on the machine.

## Decision

- The lock keeps a uid for each user and a gid for each group in `[ids.users]` and
  `[ids.groups]`, written by `lock` and `apply` like the source pins.
- A user's own `uid` wins. Otherwise a user keeps the uid the lock has, and a new one gets the
  first free uid from 1000, or from 999 down for a system user. Groups other than `root`,
  `wheel`, `users` and `nogroup` get gids from 400 the same way, the primary `group` among them.
- A user or group the configuration no longer has stays in the lock, so its number stays taken.
- A `uid` the lock keeps for someone else is an error at the option. So are two roles each giving
  a new user the same uid; locking them one at a time, or setting one, settles it.
- A switch stops before changing anything when a user's home exists and belongs to another uid,
  and says which `uid` would keep it.

## Consequences

- A wiped or freshly installed machine gets the same uids, and every machine of a role agrees.
  Nothing about accounts is kept outside the configuration and its lock.
- The plan shows a new uid as a change to `ids`.
- The lock's format is 4. Format 3 still reads, with no ids, and the first lock or apply records
  what the accounts have then. That is the numbering machines already have, except for system
  users, which move below 1000.
- Modules read the lock's ids with `ctx.locked_ids()` and record theirs with
  `ctx.base("ids", ...)`. Only `accounts` does.
