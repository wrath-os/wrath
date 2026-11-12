# 0005: Services

Status: accepted, 2026-11-07. Replaces the proposal of 2026-11-06, which had one portable service
for every platform and distro's own supervisor for projects.

## Context

Modules describe services today with `ctx.unit`, which takes systemd unit sections as they are.
Services show up in three places, which want different things:

- A machine's services, such as nginx or syncthing. On Linux these use what only systemd has:
  hardening, socket activation, timers, dynamic users, resource limits. A Mac would use launchd,
  which has its own, different features.
- A home's services, such as a sync client or an agent. Dotfiles shared between a Mac and a Linux
  machine are common.
- A project's services, such as a database and a dev server. The same `distro.toml` has to work for
  everyone on the project, on any platform, and its services start and stop with the work.

nix-darwin exposes launchd the way NixOS exposes systemd. Most NixOS service modules were never
ported to it, and those that were got rewritten for launchd. A common layer over both would hide
what makes each worth using, or keep growing escape hatches. devenv ran project processes with
process-compose, and later replaced it with a manager of its own.

## Decision

### A machine's services are its platform's

On Linux, system services are systemd units, written with `ctx.unit` as today. A Mac backend, if
one comes, gets launchd modules of its own. There's no portable layer for system services.

### Homes and projects share a small portable service

```toml
[services.postgres]
command = ["postgres", "-D", "$DISTRO_STATE/postgres"]
environment = { PGPORT = "5432" }
restart = "on-failure"          # or "always", "never"
needs = ["migrate"]             # other services of the same home or project
ready = "tcp:5432"              # or "started", "exit" for one-shot tasks
```

The fields are the ones every service manager has. Each place lowers them to its own manager:

| Where | Lowered to |
|---|---|
| a home on Linux | a systemd user unit |
| a home on a Mac, later | a launchd agent |
| a project, any platform | process-compose, run by `distro up` |

A home's services can add `systemd = {...}` or `launchd = {...}` for settings only that platform
has; a home that uses one is for that platform, and others get an error when planning. What a
platform can't express is an error when planning, never silently dropped.

### Projects run their services with process-compose

`distro up` writes process-compose's configuration from the project's services and runs
process-compose, which comes from nixpkgs like any other package. Nobody writes process-compose's
format; it's an implementation detail, and can be replaced by distro's own supervisor later
behind the same services if it gets in the way. Services stop when `distro up` ends.

## Consequences

- The `user-services` module moves to the portable form, so a home's services and a project's
  are written the same way. System modules keep `ctx.unit`.
- A module that wants a service both on a machine and in homes or projects writes it twice, once
  as a unit and once in the portable form. None do today.
- Timers stay systemd-only for now.
- Projects get `services`, and `distro up` joins the commands that work in a project.

## Alternatives

- One portable service for every platform, including system services (the earlier proposal).
  One way to write services, but it hides systemd's features or keeps growing escape hatches, and
  system services are the place where those features matter.
- Per-platform services everywhere, as NixOS and nix-darwin do. A project would need its services
  written once per platform, which doesn't work for a team on mixed machines.
- distro's own supervisor for projects from the start. Not a second format either, but weeks of
  work that process-compose already does.
