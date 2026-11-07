# 0006: Homes and user configurations

Status: accepted, 2026-11-07.

## Context

A system configuration describes users' homes today: `users/john.toml` holds john's packages,
files, git settings, services and session variables, and `distro apply` as root puts them in
place. That's what NixOS with home-manager as a module does, and it suits a machine you own.

It doesn't cover a machine you don't administer, or macOS where there may be no system
configuration at all. home-manager answers that with a standalone mode, and the two modes
differ in small ways that confuse people: different commands, different generations, options
that only work in one of them. We want to avoid that.

`distro add --home john jq` (added with `add`) edits john's packages in the system
configuration. Once standalone user configurations exist, there would be a second way to give
john a package, which is the split we want to avoid.

## Proposal

A home is described by a **user configuration**, and there's only one format for it: what
`users/john.toml` holds today. It can live in one of two places:

- inside a system configuration, as `users/<name>.toml`, managed by whoever manages the machine;
- on its own in the user's `~/.config/distro`, managed by the user, without root.

Each home has exactly one owner. A system configuration that describes john's home owns it; if
john also has his own configuration, `plan` and `apply` refuse with a message naming both. To
hand a home to its user, the system configuration keeps the account and drops the home settings,
or says so explicitly with `users.john.home = "own"` (name to be decided).

Commands work the same in both places; the context decides where they write:

| You run | Context | Writes |
|---|---|---|
| `distro add jq` | your own configuration | `~/.config/distro` |
| `sudo distro add --home john jq` | the system | `users/john.toml` in `/etc/distro` |
| `distro add jq` | the system | the system's `packages` |

A standalone user configuration has its own generations under `~/.local/state/distro`, and
`plan`, `apply`, `history` and `rollback` work on them the same way. Its tiers stop at "next
login", since nothing it changes needs a reboot.

## A baseline for a home

A system baseline that a user adds to works through 0007: the user's configuration includes the
baseline as defaults, keeps what they like and changes the rest. The user's configuration is the
starting file, so one owner still decides what goes in.

## Questions

- The name of the "the user manages this home" setting.
