# How it works

This page walks through what happens between editing a TOML file and the machine running it, and
why things are laid out the way they are. You don't need any of it to use wrath, but it helps
when something surprises you.

The short version:

```text
TOML files ──> model ──> plan ──> build ──> generation ──> switch
            (modules)  (diff)   (store)    (a system)    (activate)
```

## The configuration and the model

A configuration is a directory of TOML files. On a machine it's `/etc/wrath`, and anywhere else
you point at one with `-C DIR`. It usually looks something like this:

```text
/etc/wrath/
├── system.toml        # where it starts
├── services/web.toml  # every file in services/ is read
├── users/john.toml    # configures users.john
├── secrets/           # encrypted with age
└── wrath.lock         # pins, written for you
```

wrath reads `system.toml`, every `*.toml` under `services/`, each `users/NAME.toml` and anything
those include, into one set of options. Each option has a type and exactly one value. If two
files set `system.hostname` differently you get an error pointing at both, so there's never a
question of which one won. A value can also be a default, `{ default = "..." }`, which anything
else may override, or forced, `{ value = "...", force = "why" }`, which replaces what others set
and needs a reason. `wrath why system.hostname` tells you where a value came from.

Modules turn options into what a system is made of: packages, files in `/etc`, systemd units,
users. They come from [grapes](https://github.com/wrath-os/grapes), with the recipes, and are
written in Starlark, which is basically a small Python without the parts that make
evaluation unpredictable. The ssh module reads `services.ssh.*` and writes `sshd.service` and
`/etc/ssh/sshd_config`, the users module writes `/etc/passwd`, and so on.

The result is the model: every option's value and everything the modules produced, with nothing
left to compute. It's plain data, so its hash names the system it describes. Two machines with
the same model hash run the same thing.

## The lock

`wrath.lock` sits next to the configuration and pins everything that could otherwise change
under you:

- grapes, the source named `grapes`, which decides every package's recipe and the modules
- wrath itself
- any other source you pull files or recipes from
- each user's uid and each group's gid, so adding a user never renumbers the others

`check`, `apply` and `add` pin anything new. Only `wrath update` moves a pin forward. You don't
declare grapes yourself: the lock pins it the first time a command reads the configuration, and
`--source grapes=DIR` builds from a checkout of it for one command instead.

wrath is pinned once you run `wrath update wrath`, and from then on every system the
configuration builds carries that version of wrath, built from its source like any package.
Without a pin, a system carries the wrath that built it. `--source wrath=DIR` builds wrath from a
checkout instead, which is how you try a change to wrath on a machine.

## The plan

`wrath plan` compares the model you'd get from your TOML with the model of the running system,
or with nothing on a machine that doesn't run wrath (`--against DIR` compares with another
configuration). Each change gets a tier, which says when it takes effect:

| Tier | Takes effect |
|---|---|
| live | right away, by restarting or reloading what changed |
| next-login | when users log in again, such as their shell or environment |
| soft-reboot | after `systemctl soft-reboot`, without the kernel restarting |
| reboot | after a reboot, for the kernel, firmware and boot settings |

`apply` shows you that plan and asks before doing anything.

## Building

Every package is built from source by wrath's own builder. A recipe says where the source is and
how to build it, and most recipes just name a build style (meson, cmake, autotools, cargo, go,
zig) and let it write the script:

```toml
[[package]]
id = "grim"
version = "1.5.0"
scope = "runtime"
deps = ["pixman", "libpng", "libjpeg-turbo", "wayland", "wayland-protocols"]
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

Builds start from a seed: a small, pinned set of prebuilt compilers. From there wrath builds its
own gcc, glibc and binutils, and everything else builds with those. Rust and Go are still
prebuilt for now, which I'd like to fix at some point.

Packages whose dependencies are built go side by side, a few at a time, each as soon as what it
needs is there. A package's key, which decides whether it's built already, comes from its recipe,
the lock beside it and its dependencies, so working out what a system needs reads no source and
downloads nothing that's built.

What wrath does to a package after its build (pointing its programs at their libraries in the
store, splitting it into outputs, naming it by its contents) is a step of its own.
What the build made is kept, compressed, beside the store, so when a newer wrath changes that
step it does it again to each package in seconds instead of building everything again.
`wrath clean` removes what's kept once nothing made from it is left; `--raw` removes all of it.

### The store

A finished package goes into `/wrath/store` under a name made from a hash of what it contains:

```text
/wrath/store/d548c53eb6e631cf43328ef13bc3e841ae741852-jq-1.8.1/
├── bin/jq
└── lib/libjq.so.1 -> ...
```

On a wrath system the store is `/wrath/store`. On a machine that doesn't run wrath, it's
`build/work/store` in the wrath checkout, which builds and the programs `wrath run` starts see
as `/wrath/store`.

The hash covers every file, its permissions and every directory, with the package's own path
masked out. The same contents always land at the same path, so building something twice costs
nothing the second time, and a change to a library that doesn't change what it produces stops
right there instead of rebuilding everything above it.

Packages refer to each other by these paths. A program records where its libraries are and where
the dynamic loader is, so it runs the libraries it was built with, whatever else is on the
machine. A package is also split into outputs (programs, libraries, headers, docs), so a running
system only carries the parts it uses.

### A binary cache

A machine with a fast sibling doesn't have to build what the sibling built. Make a key on the
machine that builds, and push what it built into a directory:

```sh
wrath cache key home-1 ~/.config/wrath/cache-key --url http://builder:8080
wrath cache push /srv/wrath-cache --key ~/.config/wrath/cache-key
```

`cache key` prints the table a machine trusts the key with. Paste it into the machine's
configuration, and its builds fetch what they would otherwise build, from the directory or from
whatever serves it over HTTP, such as wrath itself:

```sh
wrath cache serve /srv/wrath-cache --listen 0.0.0.0:8080
```

```toml
[caches.home-1]
url = "http://builder:8080"
keys = ["home-1:4f0c..."]
```

Each package is looked up by the same key that decides whether it's built already, so what comes
from the cache is what this machine would have built. What's fetched is checked against the
digest the signer gave before it reaches the store, so the cache itself, and whatever serves it,
needs no trust. A package the cache doesn't have is built as usual. Only what runs comes into
the store: what built it stays a few lines in the cache's trace, unless something has to be built
with it here.

A plan says what getting there takes, asking the caches for traces and nothing else:

```text
to get there: fetch 122 paths from home (458 MB), build 1 (wrath)
```

A cache that can't be reached is said once and left out for the rest of the command, and what it
has is built instead. `--offline` asks no cache at all.

`cache push` also puts the seed there, named by its hash. A machine that has to build something
no cache has, and has no seed, fetches the seed from a cache it uses and checks it against the
hash grapes pins, so any machine with a cache can build.

Something built that you expected fetched says why: `build 3 (wrath; 2 built with it)` names what
no cache has and counts what's only built because it builds with that. `wrath cache check PKG`
goes through one package: its key, whether it's here, what it would fetch from where, or what it
builds with that no cache has, and which caches were asked.

On a machine wrath installed, fetched paths go straight into `/wrath/store`, beside the system
that runs them, and the builder keeps its records in `/wrath/work`.

Only a system's configuration trusts keys. A project can list a cache by its `url`, and what it
holds is fetched when a key the command trusts signed it. For one command, `--cache URL` and
`--cache-key KEY` add a cache and a key to those the configuration gives.

## Generations

A generation is a built system: one directory in the store that holds or links everything the
machine needs.

```text
/wrath/store/9a2ef0f8...-system/
├── bin/            # every program, linked from the packages
├── etc/            # what goes in /etc
├── kernel, initrd  # what boots it
├── init            # what the initrd starts
└── activation.json # what switching to it does
```

Generations are numbered in `/wrath/system/generations`, and `current` beside them points at the
one the machine boots by default. `wrath history` lists them, `wrath history 7` shows what
changed in generation 7, and `wrath rollback` goes back one.

## Where things live on a running machine

A conventional distribution keeps everything in `/usr` and changes files there in place. wrath
keeps everything in `/wrath/store`, which nothing changes after the fact, and points at it from
a few well-known places:

| Path | What it is |
|---|---|
| `/wrath/store` | every package and generation, read-only |
| `/wrath/system` | the numbered generations and which one is current |
| `/run/system` | the running generation |
| `/run/system-booted` | the generation that booted, which differs from the running one after a live switch |
| `/run/wrath/wrappers/bin` | setuid copies of `sudo`, `su` and friends, made at boot, since nothing in the store is setuid |
| `/etc` | real files where a program needs to write, and links through `/etc/.wrath` for the rest |
| `/bin/sh`, `/usr/bin/env` | the only two paths outside the store that scripts get to assume |
| `/var`, `/home` | your data, which wrath leaves alone |

Everything wrath keeps on disk is under `/wrath`, so a root erased at every boot keeps that one
directory, and the store holds nothing but packages and systems.

`PATH` is `/run/wrath/wrappers/bin:/run/system/bin`, and units name programs as
`/run/system/bin/sshd`. `/run/system` is in `/run` on purpose: it's per boot, so after a crash it
can't point at a half-switched system.

There's no `/usr` beyond `/usr/bin/env`. That's a deliberate choice. A `/usr` would be a second way
to find things, and builds would start depending on it again.

## Switching

Switching to a generation is done by that generation's own copy of wrath, so an older wrath
never has to understand a newer system or the other way round. It goes roughly like this:

1. Compare the old and new generation's units, and also the packages behind them, so a service
   whose program changed restarts even if its unit file looks the same.
2. Stop the units that are going away.
3. Switch `/etc` in one step, by pointing `/etc/.wrath` at the new generation.
4. Point `/run/system` at the new generation.
5. Create users and groups, setuid wrappers, and the directories systemd-tmpfiles is told about.
6. Reload systemd, restart and reload what changed, and start what's new.

A few things are never restarted by a switch. Mounts and swap would disappear, and a login manager
would take your session with it. Those take effect at the next boot, and the plan says so. A unit
can ask for the same treatment with `X-RestartIfChanged=false`.

`wrath try` switches without making the new system the default and switches back by itself after
five minutes, unless you run `wrath keep`. That's for changes that could lock you out, such as
network or SSH settings on a remote machine.

## Booting

Each generation boots from one file on the ESP, a unified kernel image: systemd's EFI stub with
the generation's kernel, initrd and kernel command line inside, put together on the machine when
it switches. Every generation gets a systemd-boot entry, with the current one as the default.
New entries count their boot attempts, and a generation that fails to boot three times in a row
stops being the default, so a broken update falls back to the last one that worked. The ESP fills
up eventually, so the oldest generations lose their entries first.

## What it doesn't manage

wrath manages the system and leaves your data alone: `/home`, `/var` and anything a service
keeps in `/var/lib`. That also means a rollback doesn't roll back data. If a service migrated
its database when it upgraded, going back to the old version won't undo that. I haven't solved
that yet.
