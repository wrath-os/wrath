# Ideas

Ideas that belong to later milestones. Add a line here instead of building them now.

- Project dev shells from a `distro.toml`: packages, environment variables and services, using the
  same package names and lock as the system. Take devenv's ergonomics (direnv, processes,
  per-project services) but keep Nix behind the backend, so a project file never contains Nix.
  Project services lower per platform: systemd-run user units on Linux, launchd agents on macOS,
  rc.d or daemon(8) on the BSDs, with a small supervisor built into distro as the fallback.
- Machine policy: checks installed on a machine with its key (`distro install --policy`), such as
  "the firewall is on", "only these ports" or "nothing in `kernel.sysctl` is forced". The machine
  checks every configuration it's given after merging, so nobody who writes the configuration can
  get around it, and errors point at the TOML line that broke the rule. Policy doesn't change how
  values merge. For fleets and Talos-like appliances; a single-owner machine doesn't need it.
