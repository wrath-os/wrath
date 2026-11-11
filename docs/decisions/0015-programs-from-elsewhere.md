# 0015: Running programs built elsewhere

Status: accepted, 2026-11-11.

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
libraries = ["sdl2"]    # beside the usual ones
```

- The generation gets `lib64/ld-linux-x86-64.so.2`: a copy of glibc's loader from the store, with
  the path of the cache it reads, `/etc/ld.so.cache`, changed in place to `/run/ld.so.cache`, a
  path of the same length. Nothing else in it changes.
- The generation gets `foreign/ld.so.cache`, which glibc's `ldconfig` makes when the generation is
  built, from the library directories of the packages listed.
- Activation links `/lib64` to the running generation's `lib64`, and `/run/ld.so.cache` to its
  cache, at boot and on every switch, and removes both when the generation has neither.
- A program from elsewhere runs as itself: the kernel starts the copied loader, which finds its
  libraries through the cache. Nothing goes into the environment, so nothing reaches what it
  starts, and `/proc/self/exe` is the program, so programs that start themselves again, as
  Chromium and Electron do, work.
- Programs distro builds name the store's loader, which reads `/etc/ld.so.cache`, and there is
  none, so they never see the cache.
- A usual set of libraries comes without listing: glibc's, the C++ runtime, zlib, OpenSSL and
  curl, and with a desktop, Wayland, Mesa, fontconfig, FreeType, X11 and ALSA. `libraries` adds
  more.
- Nothing appears in `/usr` or `/lib`. Programs that open data by fixed paths, such as
  `/usr/share/fonts`, still miss them. That's the case for a namespace run, which this decision
  leaves for later.

Off by default. A system that doesn't need it has nothing at `/lib64`. A plan shows the option
like any other, on the `live` tier.

## Consequences

- One more path outside the store, named in the configuration. The rule that store systems need no
  FHS paths holds for everything distro builds.
- The libraries are ours, built from our recipes, so a program built against a newer glibc than
  ours fails with glibc's usual version error, which says what's wrong.
- A library a program needs and the cache lacks fails with the loader's usual message, naming it.
- Tests: a program linked against `/lib64` with no run path fails without the option, and with it
  runs, finds its C++ runtime, and sees itself as `/proc/self/exe`.

## Alternatives

- A loader of our own that execs glibc's with `--library-path`, as this decision first proposed.
  It keeps the environment clean, but `/proc/self/exe` becomes glibc's loader, and programs that
  start themselves again by that path break.
- glibc's loader at `/lib64` unchanged, with `/etc/ld.so.cache`. Every program on the machine,
  including ours, would then consult a cache.
- An environment variable for the library path, as nix-ld does. It leaks into children, which
  then load the wrong libraries when they're our own programs.
- Only a namespace that looks like a conventional distribution, as buildFHSEnv does. That covers
  more programs, but needs a wrapper command for each, and nothing runs when started directly.
  It fits as a second step for the programs the loader doesn't cover.
- Patching each program's interpreter and run path on download, as patchelf does. Works for one
  program at a time, but changes the file and breaks signatures and self-updates.
