# 0016: Where a system keeps things

Status: accepted, 2026-11-10. Where the store, the generations and the per-boot directories are
changed in 0039.

## Context

A system from distro's own store has no `/usr` and runs everything from `/store`. It still
needs a few paths outside the store, and so far it uses the ones NixOS uses, because the
activator was written for systems the Nix factory built. They were never chosen for distro.
Modules, the activator, the builder and the tests now name them in a few dozen places, and each
new recipe and module adds more, so this is the time to choose.

What a system needs, apart from `/store`:

| Need | Why | Today |
|---|---|---|
| The running system, by one name | Units, `PATH`, PAM and scripts point at it, and it must not change with generation numbers. Per boot, so in `/run`. | `/run/current-system` |
| The booted system | After a live switch the running system and the kernel that booted differ; kernel modules must match the kernel. | `/run/booted-system` |
| One directory of the system's programs | `PATH`, and units that name a program by path. | `<system>/sw/bin` |
| One directory of PAM modules | PAM loads modules by path. | `<system>/sw/lib/security` |
| setuid and setcap copies | The store is read-only and owned by root without special bits. | `/run/wrappers/bin` |
| /etc | Files from the system that switch together. | links through `/etc/static` |
| A user's own packages | Their programs on their `PATH`. | `/etc/profiles/per-user/<user>` |
| The generations | Which systems the machine has, and the current one, kept with the store. | `/store/.system` |
| What scripts assume | `#!/bin/sh` and `#!/usr/bin/env` in software we don't patch. | `/bin/sh`, `/usr/bin/env` |

`sw` stands for system-wide, from Nix profiles. `/etc/static` and `/etc/profiles` are NixOS's.
`/usr/bin/env` quietly contradicts the rule that a system has no `/usr`.

## Decision

- `/run/system` is the running system and `/run/system-booted` the one that booted. Shorter,
  and says what it is. A generation is a system, so there's no "current" to qualify.
- A generation keeps its programs at `bin` and its PAM modules at `lib/security`, at its top,
  dropping `sw`. `PATH` becomes `/run/wrappers/bin:/run/system/bin`. A generation's other top
  level names (`etc`, `kernel`, `initrd`, `init`, `activation.json`) don't collide with these.
- `/run/wrappers/bin` stays: it says what's in it, and wrappers are per boot.
- `/etc` keeps linking through a single link that switches in one step, named `/etc/.system`
  instead of `/etc/static`, so it sorts away from real files and says whose it is.
- A user's packages are at `/run/system/users/<user>/bin`, part of the system, instead of a
  directory under `/etc` that holds no configuration.
- Generations stay in `/store/.system`, as now.
- `/bin/sh` and `/usr/bin/env` are named here as the only paths a system makes for software that
  hardcodes them, with `/lib64` for programs from elsewhere when 0015 is enabled. Nothing else
  appears in `/bin`, `/lib` or `/usr`.

Nix-built systems keep their paths until the Nix code is deleted. The activator handles both
until then.

## Consequences

- Modules change their paths once: `PATH` in the environment and PAM modules, `ExecStart` lines
  that name programs, polkit, fonts and graphics. Tests that check paths change with them.
- Software that guesses the old paths, such as `/run/current-system/sw/share`, won't find them.
- Documentation, the handbook and error messages name the new paths.

## Alternatives

- Keep the inherited names. Familiar to some and no work now, but distro is its own system, and
  every inherited name is one more thing to explain.
- A merged view at `/usr` for compatibility, as a conventional distribution has. Programs would
  find each other without paths, but `/usr` would become a second way of naming what's in the
  store, and builds could start depending on it again, which the store was built to prevent.
- `/system` on the root for the running system, as an earlier version of the store had. Not per
  boot, so it can be stale after a crash, and a root erased at every boot loses it.
