# 0031: A switch does only what changed, and proves it

Status: proposed, 2026-11-12.

## Context

Changing a setting, such as a theme, and seeing it should take well under a second. Evaluation
takes about 40 ms on the Framework configuration; the time goes around it. On 2026-11-12 a build
of that configuration spent about 5.5 s before building anything (not explained yet, possibly
hashing the checkout for the `distro` package) and 0.7 s assembling the system. Then every switch
runs systemd-sysusers, systemd-tmpfiles, `daemon-reload`, the restarts and the users' service
managers, whatever changed.

Skipping steps is where speed would come from, and also where a switch could quietly go wrong: a
skipped `daemon-reload` after a unit changed leaves systemd running the old one. Correct comes
before fast.

## Decision

- No new command. `apply` gets fast; `apply --watch` runs it again on each save of the
  configuration and is otherwise the same.
- First, measure and explain the time before building and in assembly, and make assembly do only
  the actions whose inputs changed, which the action cache already allows.
- Each activation step declares what it reads:
  - systemd-sysusers: `/etc/sysusers.d`, `passwd`, `group`;
  - systemd-tmpfiles: `/etc/tmpfiles.d`;
  - `daemon-reload`: unit files, drop-ins and generators, the system's and its packages';
  - the users' service managers: users' units and what's enabled for them;
  - wrappers: the model's wrappers.
- A step is skipped only when everything it reads is byte for byte the same in the old and new
  generation, compared on disk, as restarts already are (unit fingerprints). A step that doesn't
  declare its inputs always runs, so missing knowledge makes a switch slower, never wrong.
- `apply -v`, and the plan, show each step as run or skipped and the input that decided it.
- A VM test switches each change twice, once skipping and once running every step, and compares
  unit states, `/etc`, accounts and the users' managers. A difference fails the test. Skipping is
  on by default only once that test covers the scenarios.
- While watching, saves update one working generation in place; stopping or `keep` makes it a
  generation. The boot menu shows the newest few generations, so neither it nor `history` fills
  up with every save. Kernel and initrd copies on the ESP are already shared between generations
  with the same boot files.

## Consequences

- A theme change is one rendered file, one action, and an `/etc/.system` move.
- Programs still have to notice: each module can say how its program picks up a changed file (it
  watches it, a signal, a command, or a restart), and the plan shows which. Whether a program
  watching a path sees a change when only a link further along moves needs testing per program.
- Per-user settings wait for homes to come back (0017); system-wide files show it first.
