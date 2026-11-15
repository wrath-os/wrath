# Ideas

Ideas that belong to later milestones. Add a line here instead of building them now.

- Project dev shells from a `wrath.toml`: packages, environment variables and services, using the
  same package names and lock as the system. Take devenv's ergonomics (direnv, processes,
  per-project services), so a project file names only packages, variables and services.
  Project services lower per platform: systemd-run user units on Linux, launchd agents on macOS,
  rc.d or daemon(8) on the BSDs, with a small supervisor built into wrath as the fallback.
- Machine policy: checks installed on a machine with its key (`wrath install --policy`), such as
  "the firewall is on", "only these ports" or "nothing in `kernel.sysctl` is forced". The machine
  checks every configuration it's given after merging, so nobody who writes the configuration can
  get around it, and errors point at the TOML line that broke the rule. Policy doesn't change how
  values merge. For fleets and Talos-like appliances; a single-owner machine doesn't need it.
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
