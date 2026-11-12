# 0031: Switches run every step, and watching applies only settings

Status: accepted, 2026-11-12.

## Context

Changing a setting, such as a theme, and seeing it should take well under a second. Evaluation
takes about 40 ms on the Framework configuration; the time goes around it. On 2026-11-12 a build
of that configuration spent about 5.5 s before building anything and 0.7 s assembling the system.
Most of it was build styles looking for files a source lacks, such as `meson.options`, which
unpacked the archive every time; misses are kept now. A build with nothing to do takes 0.12 s,
and one after a settings change 0.23 s.

Every switch runs systemd-sysusers, systemd-tmpfiles, `daemon-reload`, the restarts and the
users' service managers, whatever changed. A first draft of this decision skipped the steps whose
inputs hadn't changed. Measured in the VM, a whole switch takes about 100 ms, `daemon-reload` 70
of it, and everything but starting and restarting under 20 ms together. Skipping would save tens
of milliseconds, and it's where a switch could quietly go wrong: a skipped `daemon-reload` after
a unit changed leaves systemd running the old one.

Running the switch again on every save has dangers of its own. Someone who forgot a watcher was
running could change a core option and have it applied. Someone with changes they hadn't applied
yet who starts watching to try a theme would get those changes too.

## Decision

### Every switch runs every step

- Nothing is skipped. A switch with nothing changed runs the same steps as any other, which also
  repairs what drifted, such as a directory systemd-tmpfiles makes.
- Each switch records how long each step took in `/run/distro/switch-steps`, and `apply -v`
  shows it.
- Skipping comes back only if a real machine shows a step that's slow, and then with what the
  first draft required: each step declaring what it reads, on disk and on the machine, skipping
  only when all of it is byte for byte the same, a VM test that switches each change with and
  without skipping and compares the result, and a time running in shadow on a real machine first.

### Watching applies settings, and nothing lasts until you keep it

- `apply --watch` switches again on each save of the configuration. There's no other new command.
- It applies a save by itself only when the plan against the running system is settings: changes
  to files a module marks as settings, and nothing else. `program-settings` and theme files are
  settings; `/etc/sudoers`, PAM, `sshd_config` and the firewall aren't, since a bad edit there
  can lock you out. No package, unit, account, wrapper, secret or boot change, and no service
  restarting or reloading because a file it reads changed. Anything more and it stops, shows the
  plan, and waits for a yes, as `apply` does.
- Each applied save says when it takes effect, from how the module says its program picks up the
  file. A program that reads it only when it starts shows nothing until then, and the watcher
  says so.
- Starting is held to the same line: when the configuration already differs from the running
  system by more than settings, watching shows the plan and waits for a yes before it starts.
- Watching works like `try`: saves switch the running system without making a generation. `keep`
  makes what's running a generation; stopping without it, a crash or a reboot returns to the
  current generation. A forgotten watcher can't leave a lasting change, and neither the boot menu
  nor `history` fills up with saves.

## Consequences

- A theme change is one rendered file, one action, and a full switch of about a tenth of a
  second, on the VM at least. The Framework's numbers, with many more units, are still to come.
- What counts as settings comes from the plan's changes, which the plan already computes, so
  watch and `apply` can't disagree about what a change does.
- Programs still have to notice: each module can say how its program picks up a changed file (it
  watches it, a signal, a command, or a restart), and the plan shows which. Whether a program
  watching a path sees a change when only a link further along moves needs testing per program.
- A file a running service restarts on isn't a setting, so a watcher never restarts a service
  without asking.
- Per-user settings wait for homes to come back (0017); system-wide files show it first.
