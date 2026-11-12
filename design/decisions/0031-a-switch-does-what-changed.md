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

Running the switch again on every save has dangers of its own. Someone who forgot a watcher was
running could change a core option and have it applied. Someone with changes they hadn't applied
yet who starts watching to try a theme would get those changes too.

## Decision

### Every switch gets faster

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

### Watching applies settings, and nothing lasts until you keep it

- `apply --watch` switches again on each save of the configuration. There's no other new command.
- It applies a save by itself only when the plan against the running system is settings: changed
  files and nothing else. No package, unit, account, wrapper, secret or boot change, and no
  service restarting or reloading because a file it reads changed. Anything more and it stops,
  shows the plan, and waits for a yes, as `apply` does.
- The same goes for starting: when the configuration already differs from the running system by
  more than settings, watching shows the plan and waits for a yes before it starts.
- Watching works like `try`: saves switch the running system without making a generation. `keep`
  makes what's running a generation; stopping without it, a crash or a reboot returns to the
  current generation. A forgotten watcher can't leave a lasting change, and neither the boot menu
  nor `history` fills up with saves.

## Consequences

- A theme change is one rendered file, one action, and an `/etc/.system` move.
- What counts as settings comes from the plan's changes, which the plan already computes, so
  watch and `apply` can't disagree about what a change does.
- Programs still have to notice: each module can say how its program picks up a changed file (it
  watches it, a signal, a command, or a restart), and the plan shows which. Whether a program
  watching a path sees a change when only a link further along moves needs testing per program.
- A file a running service restarts on isn't a setting, so a watcher never restarts a service
  without asking.
- Per-user settings wait for homes to come back (0017); system-wide files show it first.
