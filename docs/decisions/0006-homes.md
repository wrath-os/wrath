# 0006: Homes and user configurations

Status: accepted, 2026-11-07. Homes of their own, `home.toml`, are dropped for now by 0017.

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

## A user configuration on its own

Added 2026-11-07, with what's needed to build it.

- The starting file is `~/.config/distro/home.toml`. It holds what `users/<name>.toml` holds, with
  its own `distro.lock` and sources (0009) next to it.
- Its root is `users.<name>`, so nothing outside the home is reachable. Options under
  `users.<name>` that belong to the account, such as `shell`, `groups` and `uid`, are marked
  account-only by their modules. A user configuration setting one gets an error saying the
  machine's administrator sets it.
- A command run by a user who isn't root acts on `~/.config/distro` when it exists, unless `-C`
  or a project says otherwise. Root acts on the system.
- Generations live in `~/.local/state/distro`. Each is a built home: packages, files, user
  services and environment. Switching links the files, switches the profile and reloads the
  user's services, without root. It refuses to replace a file distro didn't put there.
- The system model records which homes the system manages. `apply` in a user configuration
  refuses when the system manages that home, naming the setting that hands it over.
- `distro init --user` pins the new configuration to the nixpkgs the system runs, so they share
  what's downloaded. After that the two are locked separately.
- A home inside a system configuration is part of the system's generation: `apply` and `update`
  switch the system and its homes together, never one without the other. The step that puts a
  home in place should be the same code in both cases. That comes after the standalone case
  works.
- Secrets are decrypted by the user when the home switches, with their own key (`~/.ssh/id_ed25519`
  unless `secret_identities` says otherwise), to `~/.local/state/distro/secrets`, readable only
  by them. A path option holding a secret gets `~/.local/state/distro/secrets/<name>`.
- Later: macOS.

## Questions

- The name of the "the user manages this home" setting.
