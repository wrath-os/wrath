# Setting up a machine

Once wrath runs a machine (see [Installing](installing.md)), everything about
it lives in `/etc/wrath`. This page is about what goes in there and the day-to-day loop of
changing it.

## The loop

It's the same three steps every time:

```sh
$EDITOR /etc/wrath/system.toml
wrath plan      # what would change, and when it takes effect
wrath apply     # shows the plan again, asks, then switches
```

`apply` builds what's new, makes a new generation and switches to it. It also writes anything
new into `wrath.lock`. Keep `/etc/wrath` in git (or jj) and you have the history of every
change you meant to make, next to `wrath history`, which has every system you actually ran.

When you're tuning something, such as a theme or your niri binds, let wrath follow along:

```sh
wrath apply --watch
```

Each save is tried at once when it only changes programs' own settings, files a bad edit can't
lock you out with. Anything more, a package, a service, sudoers, shows the plan and asks first.
Nothing becomes a generation while you watch: Ctrl-C asks whether to keep what runs, and
otherwise switches back. If the watcher dies, what it tried switches back on its own within ten
minutes, or at the next reboot. `--host NAME` watches for another machine, where sudo asks for
your password once, when watching starts.

## One file or several

`system.toml` is where a configuration starts. For a small machine it can hold everything. When
it grows, split it up. Two directories are read without being named:

```text
/etc/wrath/
├── system.toml
├── desktop.toml       # read because system.toml includes it
├── services/web.toml  # every *.toml under services/ is read
└── users/john.toml    # configures users.john
```

Every `*.toml` under `services/` sets options from the top, like `system.toml` does. Each
`users/NAME.toml` configures the user `NAME`, so it has no `[users.NAME]` header: write
`extra_groups = ["wheel"]` where `system.toml` would say `[users.john]` first. Don't include
these files: they're read anyway, and `check` says so if you do.

Any other file is read when something includes it, by its path from the configuration's root.
There are no wildcards:

```toml
# /etc/wrath/system.toml
include = ["desktop.toml"]

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

And when one file has to win over what others set, force it and say why:

```toml
[services.ssh]
port = { value = 22, force = "the office firewall only lets 22 through" }
```

`wrath why services.ssh.port` tells you which file set it, what it overrode, and the reason for
a forced value. Two files forcing different values is still an error.

## Users

A user's settings go in `system.toml` under `[users.NAME]`, or in a file of their own,
`users/NAME.toml`, without that header:

```toml
# /etc/wrath/users/john.toml
description = "John"
extra_groups = ["wheel", "video"]
authorized_keys = ["ssh-ed25519 AAAA... john@laptop"]
# what `openssl passwd -6` prints
initial_hashed_password = '$6$Hn0S2lSw$sQ0rK1pV8Z3xAqf7'
shell = "zsh"
packages = ["helix", "lazygit"]

[programs.zsh]
options = ["share_history"]
aliases = { ll = "ls -l" }
```

`wheel` can use sudo. The password hash only applies when the account is first created; change
your password with `passwd` on the machine and it stays changed. Each user gets a uid the first
time wrath sees them, recorded in the lock, so adding someone later never renumbers anyone
else. Set `uid` yourself if you need a particular one.

`shell` is `bash` (the default), `zsh`, `fish` or `nushell`; the system gets the shell and lists
it in `/etc/shells`. `programs.zsh` and `programs.nushell` write the user's own settings for
zsh and Nushell: aliases, lines to run at start, and for Nushell `settings` from its
`$env.config`, such as `show_banner = false`. A user's `environment` reaches whichever shell they
use, fish and Nushell included, since it's only names and values, written for each.

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

`wrath options` lists everything there is, with types, defaults and docs, and
`wrath options services.ssh` narrows it down. For something no module covers, write the
systemd unit yourself, with systemd's own sections:

```toml
packages = ["rsync"]

[units."backup.service"]
Service = { Type = "oneshot", ExecStart = "/run/system/bin/rsync -a /home/ /var/backup/home/" }

[units."backup.timer"]
Timer.OnCalendar = "daily"
```

A unit starts with the machine when its `Install` section says so, as systemd's do. A timer
without one gets `Install.WantedBy = "timers.target"`, so the timer above runs the backup every
day. A service of your own that should run all the time needs
`Install.WantedBy = "multi-user.target"`.

## Programs' settings

A program's settings go in its own format into the file it reads, for a user under `~/.config`:

```toml
[users.john.programs.alacritty.settings]
font.size = 12

[users.john.programs.noctalia.settings.bar.main]
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
table is a section and `true` is a flag. What niri writes on a node, such as a window rule's
`match = { app-id = "firefox$" }` or a bind's `allow-when-locked = true`, is written as a table
too, and a key can carry arguments, as `output "eDP-1"` does. niri applies a saved change by
itself, so an apply that only changes its settings shows up within a second.
`[programs.niri.settings]`, without a user, is for everyone, and a user's own go over it. A file
of your own where wrath would put one is left alone, and the apply says so.

### Browsers

Firefox and Zen take preferences, as `about:config` names them, for everyone or for a user, and
enterprise policies for everyone. Preferences are applied each time the browser starts, so a
change you make in the browser lasts until it restarts:

```toml
[programs.firefox.policies]
DisableTelemetry = true
DisablePocket = true

[users.john.programs.zen.settings]
browser.startup.homepage = "https://example.org"
"zen.view.compact" = true
```

They go in a profile called `default`, in `~/.config/mozilla/firefox` and `~/.config/zen`. A
`~/.mozilla/firefox` or `~/.zen` from before takes precedence, and the browser then doesn't read
wrath's: move it aside, or copy what you want to keep into the new profile.

Chromium keeps no settings file of its own a user could own, so it takes policies only, for
everyone: `policies` it enforces, and `recommended` it starts from and lets each user change.

```toml
[programs.chromium.policies]
BrowserSignin = 0
ExtensionInstallForcelist = ["cjpalhdlnbpafiamejdnhcphjbkeiagm"]

[programs.chromium.recommended]
HomepageLocation = "https://example.org"
```

## The firewall

A firewall is on by default. It drops what comes in unless it's a reply, a ping, the bits IPv6
needs to work, or a port you opened. Modules open what their services need (the ssh module opens
its port), and you open the rest:

```toml
[networking.firewall]
allowed_tcp_ports = [8080]
trusted_interfaces = ["tailscale0"]
```

It lives in its own nftables table, `inet wrath`, so tables other programs make (tailscale has
its own) are left alone. A change replaces the table in one step, with no moment in between
where nothing is filtered.

## Trying something risky

Changing the network or SSH on a machine you're not sitting at is how you lock yourself out.
`try` switches for a while and then back on its own:

```sh
wrath try --for 5m
# still reachable? keep it
wrath keep
```

If you can't get back in, just wait. After five minutes the machine switches back to what it was
running. A reboot does the same, since the tried system was never made the default.

## Going back

```sh
wrath history       # every generation, with what changed in each
wrath history 12    # the full plan for generation 12
wrath rollback      # back to the one before
wrath rollback 9    # back to generation 9
```

If a switch fails partway, say a service won't start, the new generation is still current and
boots next. wrath says so, and `wrath rollback` takes you back. Or fix what failed and run
`wrath apply` again: until the switch goes through or the machine restarts, `wrath status` says
it's unfinished, and the next apply plans from where it started, so it does all of it again.
When a rollback itself fails partway, `wrath rollback N` with the generation it names tries that
one again.

Every generation also has an entry in the boot menu, so if a change breaks booting you can pick
the previous one there. A generation that fails to boot three times stops being the default on
its own.

`wrath clean` removes old generations and what only they used. It keeps the current one, the
one running and the one that booted (after a boot fell back, those differ), plus the three before
the current one by default, and asks first. Anything copied into the store in the last hour stays
too, in case a switch is about to use it.

## Starting clean at every boot

With `state.path` set, `/` is memory and is erased at every boot. Only what `[state]` lists is
kept, under that path, which has to be a mount of its own (a btrfs subvolume, say):

```toml
[state]
path = "/persist"
directories = ["/var/lib/tailscale", "/var/backup"]
files = ["/etc/machine-id"]
```

A user's file lists what to keep in their home the same way, with paths inside it:

```toml
# users/john.toml
[state]
directories = [".ssh", "src"]
```

`/etc/wrath` and wrath's own records are always kept, and so are users' passwords. The key
secrets are decrypted with has to be listed when there are secrets, and `check` says so when
it isn't. A machine then has only what its configuration lists, so nothing piles up unnoticed.

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
installed, and `--role` picks another, such as `wrath build --role worker` to see that it still
builds. `wrath check` checks every role.

## Secrets

Secrets are encrypted with [age](https://age-encryption.org) and kept in `secrets/` next to the
configuration, so they can go in git with everything else:

```sh
wrath secret edit api-token    # opens it in $EDITOR, encrypts it again on save
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

`wrath check` evaluates the configuration and reports every error with its file and line,
without building anything. It's fast (tens of milliseconds), so it's handy in an editor hook or
before committing. In CI, `wrath check --locked` also fails when the lock is missing a pin,
instead of adding it.
