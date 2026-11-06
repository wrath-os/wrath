# 0005: Services

Status: proposed, 2026-11-06.

## Context

Modules describe services today with `ctx.unit`, which takes systemd unit sections as they are.
That's right for Linux, where distro will always use systemd. But it ties every module that runs
a program to systemd, and two things coming up don't have systemd:

- macOS, if distro grows to cover what nix-darwin does, where services are launchd jobs.
- Projects (planned), whose services (a database, a dev server) run while you work on the project.
  On Linux that could be systemd, but not on macOS or the BSDs.

If services stay systemd-shaped, every non-Linux target has to translate systemd sections, which
loses information silently, or each target gets its own way to write services, which is the kind
of split this project wants to avoid.

Others have gone two ways. devenv used process-compose for project processes and, in 2.0, replaced
it with a process manager of its own, with dependency ordering, restart policies and readiness
probes. nix-darwin exposes launchd's plist keys as options, the same way NixOS exposes systemd's,
so a service is written once per platform.

## Decision

A **service** is a concept of its own in the model, separate from systemd units:

```python
ctx.service(
    "nginx-main",
    command = ["nginx", "-c", "/etc/nginx/main.conf"],  # found on the packages' PATH
    environment = {"TZ": "UTC"},
    user = "nginx",
    restart = "always",            # or "on-failure", "never"
    needs = ["network-online"],    # other services, or a few well-known targets
    ready = "notify",              # or "started", "tcp:8080", "exit" for one-shot tasks
    restart_on = ["/etc/nginx/main.conf"],
)
```

The fields are the ones every service manager has. Each backend lowers a service to its
platform's own manager:

| Where | Lowered to |
|---|---|
| Linux, system | a systemd system unit |
| Linux, home | a systemd user unit |
| macOS, system / home | a launchd daemon / agent |
| project, any platform | distro's own supervisor |

Projects get their own supervisor on every platform, because what you want there is different
from a machine's services: they start when you start working (`distro up`, planned), stop
together when you're done, print their logs together, and need no root and nothing installed.
It's a small part of distro: dependency order, restart policies, readiness checks, logs. It reads
the same service definitions. We don't use process-compose: it would be a second configuration
format and a separate tool to install, for something we can do in a few hundred lines.

What a platform can't express is an error when planning, never silently dropped. A service that
asks for something launchd has no equivalent of fails on macOS with a message naming the field.

Two escape hatches stay, both visible in the module:

- `ctx.unit` for what only systemd has: timers, sockets, mounts, hardening options, and overriding
  units that packages ship.
- `systemd = {...}` and `launchd = {...}` on a service, for platform settings added on top of
  what the portable fields produce.

A module that uses either is marked as Linux-only or macOS-only by that use, and a configuration
for another platform that enables it gets an error when planning.

## Consequences

- The model gets `services` beside `units`. The NixOS backend lowers services to units, and
  plans show changes to services by their fields, not by unit sections.
- The service modules we have (nginx, syncthing, ollama, user services) move to `ctx.service`.
  Modules that override units their package ships (tailscale, fprintd) keep `ctx.unit`.
- `user-services` becomes services in a home, so a project's services and a user's services are
  written the same way.
- The supervisor is a new piece of code to own, kept to what the service fields describe. If it
  starts growing features systemd and launchd don't have, that's a sign to stop.
- Timers are left for later. They're portable too (launchd has calendar intervals), and should
  get the same treatment rather than staying systemd-only.

## Alternatives

- Keep systemd sections everywhere and translate them for launchd. One way to write services,
  but the translation is lossy and the gaps are invisible to whoever wrote the module.
- Write services per platform, as NixOS and nix-darwin do. Exact, but every module needs a copy
  for each platform, and they drift apart.
- process-compose for projects. Proven, but a second format and tool, and devenv moved away from
  it to its own manager for the same reasons.
