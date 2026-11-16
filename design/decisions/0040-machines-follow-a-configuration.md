# 0040: Machines follow a configuration

Status: proposed, 2026-11-16.

## Context

A machine applies what's in `/etc/wrath`, a copy made at install, or what `-C DIR` names. To
upgrade, someone has to put a newer configuration on the machine first, or push one with
`apply --host`. People who run NixOS from a configuration repository usually work like this:
install from a checkout, then upgrade from the repository's URL with no checkout on the machine,
and on some machines let a timer do that daily. wrath has no way to say "upgrade from there".

What others got wrong, or what people trip over:

- NixOS's `system.autoUpgrade` with a flake usually adds `--update-input nixpkgs`, so machines
  run a lock nobody committed or reviewed, and two machines on one repository differ. Without it,
  writing the lock of a remote flake fails, which is why `--no-write-lock-file` exists.
- It fails quietly: a broken upgrade is a line in the journal, and the machine stays behind for
  weeks unnoticed.
- `allowReboot` reboots when the timer fires, whatever the person is doing.
- Which commit a NixOS machine runs is unknown unless `system.configurationRevision` is set by
  hand.
- An upgrade from the repository silently undoes a change applied from a local checkout the
  night before.
- Private repositories need a token in `nix.conf`, separate from the configuration.
- bootc and rpm-ostree do better: the machine records its origin and `bootc status` shows it,
  updates are staged for the next boot by default, and `bootc switch` changes the origin.
  GitOps tools (Flux, Argo) show the last applied revision and its error, and pausing is explicit.

## Decision

- `-C` takes a URL as well as a directory: `github:owner/repo[/ref]` and `https://` archives,
  with `//dir` for a configuration in a subdirectory (Terraform's convention), such as
  `github:johnae/machines//framework`. A configuration fetched this way is read-only: its lock
  is never written, and something it doesn't pin is an error saying to run `wrath check` where
  the repository is edited.
- Every generation records where its configuration came from: the URL with the commit and its
  date, or the directory. `status` and `history` show it.
- A configuration says where machines running it upgrade from:

  ```toml
  [updates]
  from = "github:johnae/machines//framework"
  ```

  On a machine, commands given no `-C` use the running system's `from` when it has one, and
  `/etc/wrath` otherwise. A machine installed from a checkout therefore upgrades from the
  repository without anything else being set, and changing `from` in the repository moves
  every machine that follows it.
- `wrath plan` and `wrath apply` on such a machine fetch the newest commit of `from` and compare
  it with what runs. `wrath status` stays offline and says what runs, from which commit, and
  whether that came from `from`.
- `wrath update` and `wrath add` on a machine that follows a URL refuse, saying the lock and the
  configuration change where the repository is edited.
- `wrath apply --boot` makes the new generation the one booted next, without switching to it
  now.
- Automatic upgrades:

  ```toml
  [updates]
  from = "github:johnae/machines//framework"
  automatic = "daily"     # any systemd calendar time, spread by a random delay
  switch = "auto"         # or "now", or "boot"
  ```

  `auto` switches now when every change takes effect live or at the next login, and otherwise
  does what `--boot` does, so nothing that needs a reboot half-applies under a running session.
  wrath never reboots by itself.
- Each automatic run's result is kept, and `status` shows the last one. A failure also shows at
  login, so a machine that stopped upgrading doesn't go unnoticed.
- Automatic upgrades pause while the running system came from somewhere other than `from`, such
  as a checkout applied by hand, and `status` and login say so with the date. `wrath apply` with
  no `-C` goes back to `from` and resumes them.

## Consequences

- A configuration repository is enough to manage machines, with no checkout on them and nothing
  pushed to them.
- A machine applies exactly the lock its repository has. Moving sources forward happens in the
  repository, by a person or by CI running `wrath update`, building and pushing to the cache.
- A machine following a private repository needs a token; how a configuration gives one, from
  its secrets, is still open.
- Install from a URL (`wrath install -C github:...`) comes with this, since install already takes
  `-C`.

## Built so far

- `-C URL` for `github:`, `https://` and `path:`, with `//dir`; read-only, a commit fetched once
  is kept by its hash, so checking an unchanged repository costs one API call. A configuration
  read from a URL keeps the wrath its lock pins, and nothing is written when the lock would come
  out the same.
- Where each generation's configuration came from, in `/wrath/system/from/N`, passed through
  `apply --host` too; `status`, `history` and their `--json` show it.
- grapes' `updates` module: `updates.from`, `updates.automatic` and `updates.switch`, with errors
  at the TOML line, and `wrath-upgrade.timer` running `/run/system/bin/wrath apply --automatic`.
- On a machine following a URL: plain `plan`, `apply`, `try`, `check`, `build`, `why` and
  `options` fetch it; `add`, `remove` and `update` refuse; `status` stays offline.
- `apply --boot`, and `--automatic` with `switch`, the pause, and the last outcome in
  `/wrath/system/upgrade.json`, shown by `status`.
- In a VM following `path:`: the timer is active; an automatic upgrade fetches from the cache and
  switches; a checkout applied by hand pauses the next one, and plain `wrath apply` goes back and
  resumes them; `add` refuses; `--boot` leaves the running system alone, and `status` says which
  generation runs and which boots next; a failed automatic upgrade is kept and shown.
- Not yet: the failure shown at login, and rebooting into what `--boot` staged, in a VM.
- Found on the way: a machine knows what its store holds by the records in its work directory or
  a cache's traces, and an image or install brings no records. A machine booted from an image
  therefore rebuilds packages it already has, wrath among them, unless a cache has their traces.
  Images and install should bring the records of everything they put in the store.

## Open

- The token for private repositories, given from a secret the configuration declares.
- Upgrading only when nothing needs building (`[updates] build = false`), using what the plan
  says it would fetch and build.
- A reboot window for changes that wait for one.
- Rolling out to many machines in stages, with canaries.

## Alternatives

- Recording `from` on the machine only (bootc's origin). The configuration couldn't say it, so
  a machine installed from a checkout would need a flag, and moving machines to another URL
  would mean visiting each one.
- Pushing instead of pulling (Talos, `apply --host`). It stays for one-off changes, but a fleet
  then needs something that pushes on a schedule and knows every machine.
- Following the newest grapes on the machine (NixOS's `--update-input`). Machines would run
  locks nobody reviewed.
