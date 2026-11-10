# 0015: Running programs built elsewhere

Status: proposed, 2026-11-10.

## Context

Systems from distro's own store have no `/usr` and no `/lib64`. Every program the store builds
names its loader and libraries by store path, so none of them needs either.

Programs built for other distributions do. A dynamically linked binary downloaded from a
project's releases asks the kernel for `/lib64/ld-linux-x86-64.so.2`, and the loader then looks
for `libc.so.6`, `libstdc++.so.6` and the rest in `/lib`, `/usr/lib` and the directories in
`/etc/ld.so.cache`. On a store system the first step fails, and the shell reports "No such file
or directory" for a file that is plainly there. Statically linked programs, such as most from Go
and some from Rust, already work.

People will want to run such programs: a vendor's CLI, a language toolchain's prebuilt
compiler, a game. NixOS answers with nix-ld, a loader at `/lib64` that reads a library path from
an environment variable the system sets, and with steam-run and buildFHSEnv, which run a
program in a namespace that looks like a conventional distribution.

## Decision

A system can opt in to running programs built elsewhere:

```toml
[programs.foreign]
enable = true
libraries = ["libstdc++", "openssl", "zlib", "libxcrypt"]
```

- The generation gets `lib64/ld-linux-x86-64.so.2`, a small loader of our own, and distro links
  `/lib64` to it at boot and on every switch. It's the only thing a store system adds outside
  `/store`, `/etc`, `/run` and the user directories.
- The loader execs glibc's real loader from the store with `--library-path` set to the
  generation's `foreign/lib`, which links the libraries of the packages listed, and passes the
  program and its arguments through. The program sees no change in its environment, so what it
  starts doesn't inherit a library path meant for it.
- Nothing appears in `/usr` or `/lib`. Programs that open data by fixed paths, such as
  `/usr/share/fonts` or `/usr/lib/locale`, still miss them. That's the case for a namespace run,
  which this decision leaves for later.
- A plan shows the option like any other, and `why /lib64/ld-linux-x86-64.so.2` names it.

Off by default. A system that doesn't need it keeps having nothing at `/lib64`.

## Consequences

- One more path outside the store. The rule that store systems need no FHS paths holds for
  everything distro builds, and the exception is named in the configuration.
- The libraries listed are ours, built from our recipes, so a program built against a newer glibc
  than ours fails with glibc's usual version error. That error says what's wrong.
- Programs that check for `/usr/bin/env` or `/bin/bash` already find them, since distro links
  both.
- Tests: a static program runs without the option; a dynamic one fails without it with a message
  that names the option; with it, the same binary runs, and a child it starts doesn't see the
  library path.

## Alternatives

- Link glibc's loader at `/lib64` directly and write `/etc/ld.so.cache`. Simpler, but every
  program on the machine would then consult a cache that names store paths, including ours,
  which so far never do.
- An environment variable for the library path, as nix-ld does. It leaks into children, which
  then load the wrong libraries when they're our own programs.
- Only a namespace that looks like a conventional distribution, as buildFHSEnv does. That covers
  more programs, but needs a wrapper command for each, and nothing runs when started directly.
  It fits as a second step for the programs the loader doesn't cover.
- Patching each program's interpreter and run path on download, as patchelf does. Works for one
  program at a time, but changes the file and breaks signatures and self-updates.
