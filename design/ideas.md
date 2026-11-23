# Ideas

Ideas that belong to later milestones. Add a line here instead of building them now. The large
directions, and the products the core has to allow, are in `vision.md`.

- Project dev shells from a `wrath.toml`: packages, environment variables and services, using the
  same package names and lock as the system. Take devenv's ergonomics (direnv, processes,
  per-project services), so a project file names only packages, variables and services.
  Project services lower per platform: systemd-run user units on Linux, launchd agents on macOS,
  rc.d or daemon(8) on the BSDs, with a small supervisor built into wrath as the fallback.
- Machine policy: checks installed on a machine with its key (`wrath install --policy`), such as
  "the firewall is on", "only these ports" or "nothing in `kernel.sysctl` is forced". The machine
  checks every configuration it's given after merging, so nobody who writes the configuration can
  get around it, and errors point at the TOML line that broke the rule. Policy doesn't change how
  values merge. For fleets and immutable appliances; a single-owner machine doesn't need it.
- Settings changes under a second: change a theme in the TOML, save, and the editor or niri shows
  it right away. Evaluation already takes about 40 ms on the Framework config; the time goes
  around it. On 2026-11-12 a build of that config spent about 5.5 s before building anything
  (not yet explained, possibly hashing the checkout) and 0.7 s assembling the system. What it
  takes, for a change that only touches files:
  - Assembly does only what changed: the one rendered file's action runs, everything else is a
    cache hit, and putting the system together costs milliseconds.
  - A files-only switch. Today every switch runs sysusers, tmpfiles, `daemon-reload`, restarts and
    the user managers. When the plan says only files changed, moving `/etc/.wrath` is the whole
    switch.
  - Programs notice. A module says how its program picks up a new file: it watches it (niri,
    alacritty), a signal (helix on `SIGUSR1`), a command, or a restart. The plan shows which.
    Unknown: whether a program watching a path with inotify sees a change when only a symlink
    further along the chain moves. Test per program; a direct link or a touch may be needed.
  - No generation per save. `wrath try --watch` keeps one working generation and updates it in
    place on each save; `keep` makes it a generation and stopping goes back. Same verbs as today.
  - Per-user settings such as an editor theme wait for homes to come back. System-wide files
    (`/etc/xdg`, niri's system config) are enough to prove it.
  Changes to packages or units stay slower; that's fine.
- `wrath set services.nginx.port 8080`: changes a setting where the configuration lives. The
  value's origin, as `wrath why` finds it, says which file and line to edit; a value set nowhere
  goes in the machine's own file. TOML comments and layout are kept, and plan and apply follow as
  usual. A machine following a URL refuses, as `add` and `update` do, and shows the edit to make
  in the repository; later it could open a pull request there. No machine-local layer on top of
  a fetched configuration: the repository would stop describing the machine, two machines on one
  commit would differ, and every upgrade would have to merge it.
- `wrath try --set services.nginx.port=8080`: a generation from the configuration plus overrides,
  rolled back unless kept. The generation records them beside its origin. Keeping one for good
  means putting it in the configuration.
- `wrath verify`, in place of general drift detection: commands that change settings write the
  configuration, and `/etc` and the store are read-only, so what's left to check is small. It
  rehashes store paths against their names, which works on every platform, and lists files in
  `/etc` that nothing manages. To find out: what a hand-run `systemctl enable` does against the
  managed `/etc`. Which machine runs which commit is `status`'s job (0040).
- fs-verity on store paths for Linux machines, opt-in beside Secure Boot: the kernel checks each
  page as it's read, carrying the verified chain from the UKI into the store. Needs a filesystem
  that has it (btrfs, ext4); never something the design depends on, since macOS lacks it.
- macOS stores: a read-only APFS volume through `synthetic.conf`. arm64 macOS kills unsigned or
  wrongly signed executables, and rewriting store paths breaks a Mach-O signature, so the
  builder's fixup has to ad-hoc sign after rewriting before anything it builds runs there. The
  kernel checking those signatures per page covers executables; `verify` covers the rest.
