# wrath

wrath is a Linux distribution you describe in a few TOML files. You write down what a machine
should be - packages, users, services, disks, settings - and wrath builds that system and
switches the machine to it. Something like:

```toml
# /etc/wrath/system.toml
packages = ["git", "jujutsu", "ripgrep"]

[system]
hostname = "framework"

[users.john]
extra_groups = ["wheel"]

[services.ssh]
enable = true
```

```console
$ wrath plan
system /etc/wrath (framework)
packages
  + ripgrep (tier: live)
1 change, tier: live
$ wrath apply
```

Every change shows you a plan first, and the plan says when each part takes effect: right away,
at your next login, or after a reboot. Every system you've switched to is kept as a generation, so
going back is `wrath rollback`.

## Why I built it

There are lots of ways to run Linux these days. Some systems are declarative, built from a
description you keep in a repository. Some are immutable, so the running system can't drift from
what was built. A few can even switch between those immutable states while running, without a
reboot. I love all of those ideas, and wrath is my take on putting them together in a way that
feels good to use every day.

What I care about:

- A configuration is TOML, easy to read and write. Set the same option to two different values
  in two files and you get an error pointing at both lines.
- Errors point at what you wrote. Modules, the code that turns options into files and services,
  are Starlark: a small, Python-like language that's type checked, so `wrath check` catches a
  mistake before anything builds.
- You see what happens before it happens: `wrath plan` lists every change and its tier, and
  `wrath apply` switches the running machine when it can and tells you when a reboot is needed.
- A machine can follow a configuration repository and upgrade itself on a timer, fetching what's
  already built from a cache. A generation that fails to boot three times falls back to the last
  one that worked.
- wrath has its own hermetic builder and store. Packages build from source in a sandbox, each
  one ends up in `/wrath/store` under a name made from what it contains, and the lock pins every
  source, so the same configuration gives you the same system.
- The same commands and the same TOML describe a whole machine or a single project's
  development environment.

The packages and modules live in their own repository,
[grapes](https://github.com/wrath-os/grapes), pinned in your lock like any other source.

## Where it is

It's early. It runs on x86_64, about 370 packages build (enough for a niri desktop, development
tools and the usual command line things), and it's running on my Framework desktop, installed from
a USB stick onto encrypted disks that unlock with the TPM. That machine follows its configuration
from a private repository and upgrades itself, wrath included.

What's still missing: a public binary cache (so for now your machine, or one of yours, builds
everything), Secure Boot (designed, being built), aarch64, and a lot of packages. The docs are
written as I go, so some pages are thin.

## Where to go next

- [How it works](how-it-works.md), if you want to know what's going on before trying it.
- [Getting started](getting-started.md), to boot it in a VM.
- [Installing](installing.md), to put it on a machine.

## Thanks

wrath is inspired by a lot of tools and the people behind them, and I've learned something from
every one of them. [Starlark](https://github.com/facebook/starlark-rust) (the Rust
implementation) runs the modules. Thank you all.
