# 0033: A home's files go through its generation

Status: proposed, 2026-11-13.

## Context

A system gives each user files under `users/<user>/files` in its generation, and the switch links
each one from the home through `/run/system`: `~/.config/wezterm/wezterm.lua` points at
`/run/system/users/demo/files/.config/wezterm/wezterm.lua`. A switch or rollback flips
`/run/system` and every home file changes with it; a home's links change only when a file is
added or removed. A file of the user's own where a link would go is left alone, with a note. Only
`users/<user>/bin` is written down (0016).

Linking each file in the home to the store by its own store path means every activation rewrites
every link, one at a time, and a file in the way has to stop activation, be moved aside, or be
forced.

Both point into a store that can't be written, which stops a program writing its file in place.
It doesn't stop a program that saves by writing a new file and renaming it over the old one: the
rename replaces the link, since the directory is the user's. VS Code's `settings.json`, GTK's
`settings.ini`, `mimeapps.list` and KDE's files do this. Here the next switch leaves the
program's file and the setting stops applying, with only a note in the switch output.

macOS needs the same files without a system that manages the home (0032), and standalone homes
(0006) are to come back.

## Decision

### Through a pointer, wherever the home is managed from

A home's files link through a stable pointer to the generation that owns the home. The pointer is
a parameter of the step that links them, which is the same code for every case (0006):

| A home managed by | Its files link to |
|---|---|
| A Linux system (today) | `/run/system/users/<user>/files/...` |
| Itself, on Linux or macOS | its current generation, such as `~/.local/state/distro/home/...` |
| A macOS system, if there is one | the system's pointer, made with `synthetic.conf` |

### Keeping a program off its configuration

Nothing can stop a rename over a link without breaking the program: a read-only directory stops
it writing anything else there, and an immutable flag needs root and makes saves fail outright.
So distro chooses where the program's writes go, and says when they went somewhere else.

1. Layers where the program has them. A settings module writes its file where the program reads
   but doesn't write, and leaves the program a file of its own. niri's user file includes the
   system's, and noctalia keeps what its settings window saves in its state directory. Ghostty's
   `config-file`, git's `include` and SSH's `Include` work the same way. This is the first choice
   for every settings module.
2. A mode per home file, for programs without layers:
   - `link`, the default: read-only, from the store.
   - `copy`: a writable copy. A switch replaces it only when it's still what distro last wrote,
     which it records; otherwise it says so and leaves it.
   - `once`: written when missing, then the program's.
3. A replaced link is reported where it's seen. The switch and `distro status` name a home file
   that was a link and is now the program's, with the `diff` that shows how it differs from the
   system's, and how to settle it: take what's wanted into the configuration, then move the
   program's aside to `.old` with the `mv` the note gives, and apply again. Nothing is
   overwritten. A program's file with the system's text in it is linked again without a word.
   The plan says it too, later, once it reads the machine's homes.

### macOS

- Settings modules take their path per target, since some programs read
  `~/Library/Application Support/...` on macOS.
- User services become launchd agents (ideas.md).
- A home's files on macOS are a standalone home's, until a macOS system exists (0030, 0032).

## Alternatives

- Links straight to store paths. Each switch then rewrites every link, and a switch cut short
  leaves a home half old and half new.
- Copies everywhere. Programs could write them, but distro would have to compare every file at
  every switch, and rollback would have to put copies back.
- Failing the switch on a replaced link. It stops a whole system switching because of one
  program's file.

- Links to a file in the configuration's checkout instead of the store, so edits apply at once
  and a program's saves show in `jj diff`. A rollback wouldn't take the file back, and it works
  only on the machine with the checkout, never through `apply --host`. `apply --watch` covers
  editing live.
- A command that puts the system's file back, or a switch that asks for each. The `mv` in the note
  does the same with nothing new to learn; either can come if that gets tedious.

## Open

- Where a standalone home keeps its generations, and the name of its pointer.
