# How it works

This page walks through what happens between editing a TOML file and the machine running it, and
why things are laid out the way they are. You don't need any of it to use distro, but it helps
when something surprises you.

The short version:

```text
TOML files ──> model ──> plan ──> build ──> generation ──> switch
            (modules)  (diff)   (store)    (a system)    (activate)
```

## The configuration and the model

A configuration is a directory of TOML files. On a machine it's `/etc/distro`, and anywhere else
you point at one with `-C DIR`. It usually looks something like this:

```text
/etc/distro/
├── system.toml        # where it starts
├── services/web.toml  # anything it includes
├── users/john.toml
├── secrets/           # encrypted with age
└── distro.lock        # pins, written for you
```

distro reads every file into one set of options. Each option has a type and exactly one value.
If two files set `system.hostname` differently you get an error pointing at both, so there's
never a question of which one won. A value can also be a default, `{ default = "..." }`, which
anything else may override. `distro why system.hostname` tells you where a value came from.

Modules turn options into what a system is made of: packages, files in `/etc`, systemd units,
users. They're written in Starlark, which is basically a small Python without the parts that make
evaluation unpredictable. The ssh module reads `services.ssh.*` and writes `sshd.service` and
`/etc/ssh/sshd_config`, the users module writes `/etc/passwd`, and so on.

The result is the model: every option's value and everything the modules produced, with nothing
left to compute. It's plain data, so its hash names the system it describes. Two machines with
the same model hash run the same thing.

## The lock

`distro.lock` sits next to the configuration and pins everything that could otherwise change
under you:

- the distro revision, which decides every package's recipe and the modules
- any other source you pull files or recipes from
- each user's uid and each group's gid, so adding a user never renumbers the others

`check`, `apply` and `add` pin anything new. Only `distro update` moves a pin forward.

## The plan

`distro plan` compares the model you'd get from your TOML with the model of the running system.
Each change gets a tier, which says when it takes effect:

| Tier | Takes effect |
|---|---|
| live | right away, by restarting or reloading what changed |
| next-login | when users log in again, such as their shell or environment |
| soft-reboot | after `systemctl soft-reboot`, without the kernel restarting |
| reboot | after a reboot, for the kernel, firmware and boot settings |

`apply` shows you that plan and asks before doing anything.

## Building

Every package is built from source by distro's own builder. A recipe says where the source is and
how to build it, and most recipes just name a build style (meson, cmake, autotools, cargo, go)
and let it write the script:

```toml
[[package]]
id = "grim"
version = "1.5.0"
scope = "runtime"
core = true
extra_deps = ["pixman", "libpng", "libjpeg-turbo", "wayland", "wayland-protocols"]
sources = [{ url = "https://gitlab.freedesktop.org/emersion/grim/-/archive/v{version}/grim-v{version}.tar.gz" }]
smoke = ["bin/grim -h"]

# Screenshots, of the whole screen or of the region slurp picks.
[package.meson]
args = ["-D jpeg=enabled", "-D man-pages=disabled"]
```

Each build runs in a sandbox (bubblewrap) with no network, a fixed umask and a `/usr` assembled
from exactly the dependencies the recipe lists. So a build can't quietly pick up something that
happens to be installed, which is the most common way builds stop being reproducible. If a
dependency is missing, the build fails and the hint says which recipe has it.

Builds start from a seed: a small, pinned set of prebuilt compilers. From there distro builds its
own gcc, glibc and binutils, and everything else builds with those. Rust and Go are still
prebuilt for now, which I'd like to fix at some point.

### The store

A finished package goes into `/store` under a name made from a hash of what it contains:

```text
/store/d548c53eb6e631cf43328ef13bc3e841ae741852-jq-1.8.1/
├── bin/jq
└── lib/libjq.so.1 -> ...
```

The hash covers every file, its permissions and every directory, with the package's own path
masked out. The same contents always land at the same path, so building something twice costs
nothing the second time, and a change to a library that doesn't change what it produces stops
right there instead of rebuilding everything above it.

Packages refer to each other by these paths. A program records where its libraries are and where
the dynamic loader is, so it runs the libraries it was built with, whatever else is on the
machine. A package is also split into outputs (programs, libraries, headers, docs), so a running
system only carries the parts it uses.

## Generations

A generation is a built system: one directory in the store that holds or links everything the
machine needs.

```text
/store/9a2ef0f8...-system/
├── bin/            # every program, linked from the packages
├── etc/            # what goes in /etc
├── kernel, initrd  # what boots it
├── init            # what the initrd starts
└── activation.json # what switching to it does
```

Generations are numbered in `/store/.system/generations`, and `current` points at the one the
machine boots by default. `distro history` lists them, `distro history 7` shows what changed in
generation 7, and `distro rollback` goes back one.

## Where things live on a running machine

A conventional distribution keeps everything in `/usr` and changes files there in place. distro
keeps everything in `/store`, which nothing changes after the fact, and points at it from a few
well-known places:

| Path | What it is |
|---|---|
| `/store` | every package and generation, read-only |
| `/store/.system` | the numbered generations and which one is current |
| `/run/system` | the running generation |
| `/run/system-booted` | the generation that booted, which differs from the running one after a live switch |
| `/run/wrappers/bin` | setuid copies of `sudo`, `su` and friends, made at boot, since nothing in the store is setuid |
| `/etc` | real files where a program needs to write, and links through `/etc/.system` for the rest |
| `/bin/sh`, `/usr/bin/env` | the only two paths outside the store that scripts get to assume |
| `/var`, `/home` | your data, which distro leaves alone |

`PATH` is `/run/wrappers/bin:/run/system/bin`, and units name programs as
`/run/system/bin/sshd`. `/run/system` is in `/run` on purpose: it's per boot, so after a crash it
can't point at a half-switched system.

There's no `/usr` beyond `/usr/bin/env`. That's a deliberate choice. A `/usr` would be a second way
to find things, and builds would start depending on it again.

## Switching

Switching to a generation is done by that generation's own copy of distro, so an older distro
never has to understand a newer system or the other way round. It goes roughly like this:

1. Compare the old and new generation's units, and also the packages behind them, so a service
   whose program changed restarts even if its unit file looks the same.
2. Stop the units that are going away.
3. Switch `/etc` in one step, by pointing `/etc/.system` at the new generation.
4. Point `/run/system` at the new generation.
5. Create users and groups, setuid wrappers, and the directories systemd-tmpfiles is told about.
6. Reload systemd, restart and reload what changed, and start what's new.

A few things are never restarted by a switch. Mounts and swap would disappear, and a login manager
would take your session with it. Those take effect at the next boot, and the plan says so. A unit
can ask for the same treatment with `X-RestartIfChanged=false`.

`distro try` switches without making the new system the default and switches back by itself after
five minutes, unless you run `distro keep`. That's for changes that could lock you out, such as
network or SSH settings on a remote machine.

## Booting

Each generation gets a systemd-boot entry, with the current one as the default. New entries count
their boot attempts, and a generation that fails to boot three times in a row stops being the
default, so a broken update falls back to the last one that worked. The ESP fills up eventually,
so the oldest generations lose their entries first.

## What it doesn't manage

distro manages the system and leaves your data alone: `/home`, `/var` and anything a service
keeps in `/var/lib`. That also means a rollback doesn't roll back data. If a service migrated
its database when it upgraded, going back to the old version won't undo that. NixOS has the same
gap, and I haven't solved it yet.
