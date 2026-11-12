# Setting up a machine

Once distro runs a machine (see [Installing](installing.md)), everything about
it lives in `/etc/distro`. This page is about what goes in there and the day-to-day loop of
changing it.

## The loop

It's the same three steps every time:

```sh
$EDITOR /etc/distro/system.toml
distro plan      # what would change, and when it takes effect
distro apply     # shows the plan again, asks, then switches
```

`apply` builds what's new, makes a new generation and switches to it. It also writes anything
new into `distro.lock`. Keep `/etc/distro` in git (or jj) and you have the history of every
change you meant to make, next to `distro history`, which has every system you actually ran.

## One file or several

`system.toml` is where a configuration starts. For a small machine it can hold everything. When
it grows, split it up and include the pieces:

```toml
# /etc/distro/system.toml
include = ["desktop.toml", "users/*.toml"]

[system]
hostname = "framework"
```

Included files set options exactly like the main file does, and nothing is merged behind your
back. If `desktop.toml` and `system.toml` both set `services.ssh.port` to different things,
that's an error pointing at both lines. When you want a value that something else may override,
say so:

```toml
[services.ssh]
port = { default = 2222 }
```

`distro why services.ssh.port` tells you which file set it and whether something overrode a
default.

## Users

```toml
[users.john]
description = "John"
extra_groups = ["wheel", "video"]
authorized_keys = ["ssh-ed25519 AAAA... john@laptop"]
initial_hashed_password = '$6$...'
packages = ["helix", "lazygit"]
```

`wheel` can use sudo. The password hash only applies when the account is first created; change
your password with `passwd` on the machine and it stays changed. Each user gets a uid the first
time distro sees them, recorded in the lock, so adding someone later never renumbers anyone
else. Set `uid` yourself if you need a particular one.

A user's `packages` end up on their own `PATH` only.

## Services

Most things you'd want running have a module, and enabling one is usually all there is to it:

```toml
[services.ssh]
enable = true

[services.tailscale]
enable = true

[audio]
enable = true

[desktop]
session = "niri"
```

`distro options` lists everything there is, with types, defaults and docs, and
`distro options services.ssh` narrows it down. For something no module covers, write the
systemd unit yourself:

```toml
[units."backup.service"]
Service = { Type = "oneshot", ExecStart = "/run/system/bin/restic backup /home" }

[units."backup.timer"]
Timer.OnCalendar = "daily"
```

## Programs' settings

A program's settings go in its own format into the file it reads, for a user under `~/.config`:

```toml
[users.john.programs.alacritty.settings]
font.size = 12

[users.john.programs.noctalia.settings.bar]
position = "top"

[users.john.programs.niri.settings]
prefer-no-csd = true
layout.gaps = 12
binds."Mod+Return".spawn = "alacritty"
'output "eDP-1"'.scale = 2.0
```

A config that's easier kept as a file of its own, such as a Lua one, goes in the configuration's
directory and is named with `source`, from the configuration's root:

```toml
[users.john.files."~/.config/wezterm/wezterm.lua"]
source = "wezterm/wezterm.lua"
```

niri's settings are written as KDL over niri's own defaults, so you only write what you change. A
table is a section, `true` is a flag, and a key can carry arguments, as `output "eDP-1"` does.
niri applies a saved change by itself, so an apply that only changes its settings shows up within
a second. `[programs.niri.settings]`, without a user, is what users without settings of their own
get. A file of your own where distro would put one is left alone, and the apply says so.

## The firewall

A firewall is on by default. It drops what comes in unless it's a reply, a ping, the bits IPv6
needs to work, or a port you opened. Modules open what their services need (the ssh module opens
its port), and you open the rest:

```toml
[networking.firewall]
allowed_tcp_ports = [8080]
trusted_interfaces = ["tailscale0"]
```

It lives in its own nftables table, `inet distro`, so tables other programs make (tailscale has
its own) are left alone. A change replaces the table in one step, with no moment in between
where nothing is filtered.

## Trying something risky

Changing the network or SSH on a machine you're not sitting at is how you lock yourself out.
`try` switches for a while and then back on its own:

```sh
distro try --for 5m
# still reachable? keep it
distro keep
```

If you can't get back in, just wait. After five minutes the machine switches back to what it was
running. A reboot does the same, since the tried system was never made the default.

## Going back

```sh
distro history       # every generation, with what changed in each
distro history 12    # the full plan for generation 12
distro rollback      # back to the one before
distro rollback 9    # back to generation 9
```

If a switch fails partway, say a service won't start, the new generation is still current and
boots next. distro says so, and `distro rollback` takes you back. Or fix what failed and run
`distro apply` again: until the switch goes through or the machine restarts, `distro status` says
it's unfinished, and the next apply plans from where it started, so it does all of it again.
When a rollback itself fails partway, `distro rollback N` with the generation it names tries that
one again.

Every generation also has an entry in the boot menu, so if a change breaks booting you can pick
the previous one there. A generation that fails to boot three times stops being the default on
its own.

`distro clean` removes old generations and what only they used. It keeps the current one, the
one running and the one that booted (after a boot fell back, those differ), plus the three before
the current one by default, and asks first. Anything copied into the store in the last hour stays
too, in case a switch is about to use it.

## Several machines from one configuration

Machines of the same kind can share a configuration through roles. Instead of `system.toml`,
the configuration has a file per kind:

```text
config/
├── common.toml
├── machines.toml
└── roles/
    ├── laptop.toml
    └── worker.toml
```

```toml
# roles/worker.toml
include = ["common.toml"]

[services.nginx.main]
enable = true
```

```toml
# machines.toml
laptop = { role = "laptop" }
w-01 = { role = "worker" }
w-02 = { role = "worker" }
```

Every worker runs the same system, built once. What differs between machines, like their names,
is filled in on each machine when it switches. A machine knows its role from when it was
installed, and `--role` picks another, such as `distro build --role worker` to see that it still
builds. `distro check` checks every role.

## Secrets

Secrets are encrypted with [age](https://age-encryption.org) and kept in `secrets/` next to the
configuration, so they can go in git with everything else:

```sh
distro secret edit api-token    # opens it in $EDITOR, encrypts it again on save
```

```toml
[system]
secret_recipients = ["age1..."]  # who can decrypt: the machine's key, and yours

[secrets.api-token]
owner = "syncthing"

[files."/etc/app/env"]
text = "TOKEN=${secret.api-token}\n"
```

The encrypted files go into the system as they are, and they're only ever decrypted on the
machine, into `/run/secrets`, by the keys in `secret_identities` (the machine's SSH host key by
default). A file that uses a secret, like `/etc/app/env` above, is filled in there too, so the
plain text never ends up in the store or in a build.

## Checking before applying

`distro check` evaluates the configuration and reports every error with its file and line,
without building anything. It's fast (tens of milliseconds), so it's handy in an editor hook or
before committing. In CI, `distro check --locked` also fails when the lock is missing a pin,
instead of adding it.
