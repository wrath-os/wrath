# distro

distro is a Linux distribution you describe in a few TOML files. You write down what a machine
should be - packages, users, services, settings - and distro builds that system from source and
switches the machine to it. Something like:

```toml
# /etc/distro/system.toml
packages = ["git", "jujutsu", "ripgrep"]

[system]
hostname = "framework"

[users.john]
extra_groups = ["wheel"]

[services.ssh]
enable = true
```

```console
$ distro plan
system /etc/distro (framework)
packages
  + ripgrep (tier: live)
1 change, tier: live
$ distro apply
```

Every change shows you a plan first, and says when it takes effect: right away, at your next
login, or after a reboot. Every system you've switched to is kept as a generation, so going back
is `distro rollback`.

## Why

I've used NixOS for years and I really like the idea: one description of a machine, a lock, and
rollback. But I kept running into the same things. The Nix language is hard to read and harder to
debug, an option set in two places merges in ways you have to look up, and most errors point
somewhere deep in nixpkgs instead of at the line you wrote.

So distro keeps the parts I like and tries to fix the rest:

- You write TOML. Setting the same option to two different values in two files is an error that
  points at both lines, never a silent merge.
- Modules, the code that turns options into files and services, are Starlark. That's a small,
  Python-like language, type checked, so a mistake in a module is caught when you run `check`.
- It has its own builder and store. Packages build from source in a sandbox, and each one ends up
  in `/store` under a name made from what it contains, so the same package built twice is stored
  once.
- The same commands and the same model work for a whole machine and for a single project's
  development environment.

It's really early days. It has no users yet (well, me), it only runs on x86_64, and plenty is
missing. I'm working towards using it on my own desktop daily, and these docs are written as I
go, so some pages are still empty.

## What it isn't (yet)

There's no binary cache, so everything builds on your machine. A whole desktop takes about an
hour on a fast one. There's no security team or advisory feed. Around 350 packages build, which
covers a niri desktop, development tools and the usual command line things, but it's nowhere near
what Debian or nixpkgs have.

If you need any of that, NixOS, Guix and Fedora Silverblue are all really good and much further
along.

## Thanks

distro borrows a lot. The store, the generations and rollback come from
[Nix and NixOS](https://nixos.org), and the full-source bootstrap ideas from
[Guix](https://guix.gnu.org). Talos and Fedora's bootc shaped how updates and rollback should
feel, and [Starlark](https://github.com/facebook/starlark-rust) (the Rust implementation, from
the Buck2 folks) runs the modules. Thanks to all of them.

## Where to go next

- [How it works](how-it-works.md), if you want to know what's going on before trying it.
- [Getting started](getting-started.md), to boot it in a VM.
