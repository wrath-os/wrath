# 0030: A configuration builds on a platform

Status: accepted, 2026-11-12.

## Context

Every configuration gets every built-in module, from the `modules/` next to the distro binary.
A module either runs always or behind its own `enable` option. Nothing lets a configuration build
on a different set, and the only way to bring other modules is `--modules DIR`, which replaces
the whole set and isn't recorded in the lock.

That rules out building something quite different on the same tool: an appliance without
`accounts`' sudo and PAM, a system with another network stack, a downstream distribution with
its own base. The adversarial review found the same from the other side:

- `base-system` always adds bash, openssh, less, getty and sudo, and `accounts`, `polkit`,
  `dbus`, `pam-base` and `networkd` can't be turned off.
- A file a module owns, such as `/etc/sudoers`, can't be taken over by the configuration.
- Some of the base lives in Rust where no module reaches it: `BASE_PACKAGES` (bash, coreutils,
  systemd and the rest), the init and switch scripts, branding in os-release and boot entries.

A first draft added `disable = [...]` and `replaces = "..."` for single modules. That's a second
way of saying "not this" next to `force` and `remove` (0007), and it starts from our set and
subtracts, which reads badly for a system that shares little with ours.

## Decision

- A platform is a named set of modules a configuration builds on. The starting file says which:

  ```toml
  platform = "distro"               # the default, and what every configuration uses today
  platform = { source = "acme" }    # acme's platform, pinned in the lock
  ```

- `distro` is today's built-in modules. Nothing changes for a configuration that doesn't say.
- A platform is a directory with a `platform.toml` that lists its modules: its own, by path, and
  any of distro's, by name. A module is in a system because its platform lists it, so there's
  nothing to disable:

  ```toml
  # acme's platform.toml
  modules = ["modules"]                       # acme's own directory
  from-distro = ["dbus", "polkit", "users"]   # distro's modules acme keeps
  ```

- Option names stay one namespace. Two modules in a platform declaring the same option is an
  error naming both, so a module replacing one of distro's declares the same options and must
  provide what other modules write to them. A platform leaving out a module whose options another
  module writes to, such as `networking.firewall.allowed_tcp_ports`, is an error pointing at both.
- The source is pinned in the lock like any other (0009), so the model says exactly which module
  code ran.
- The base that's in Rust moves into modules. `BASE_PACKAGES` becomes what `base-system` adds,
  except what the mechanism itself runs: distro, bash and coreutils for its scripts, systemd,
  util-linux and kmod, and D-Bus. `base-system` writes os-release from `system.name` and
  `system.id`, and boot entries take their title from it. The ESP directory stays `EFI/distro`
  for now: renaming it on a running machine means moving its kernels, which is mechanism work
  for later.
- What a system can't do without stays a check: a system with no init, no `/bin/sh` or no
  `PATH` fails `check` with what's missing, whichever modules its platform has.

## Consequences

- A downstream distribution is a source with a platform and recipes. Its users' configurations
  say `platform = { source = "acme" }`. It can pin the distro it was tested on through its own
  lock, which still needs `acme`'s lock to be read (open).
- `--modules DIR` stays for developing distro itself, and the model records that it was used.
- The plan shows a platform change, and the modules it adds and drops, like any other change.
  Dropping `accounts` takes the machine's users with it, and the plan has to say so plainly.
- Recipe ids keep one namespace for now. Two sources declaring the same recipe id is an error
  naming both; namespacing recipes by source (`acme#jq`) is separate.

- A platform builds on distro's modules only, never on another platform. A chain of platforms is
  the layering 0007 left out: where a module came from would take walking the chain. Taking
  modules by name from another source, without its platform, can come later if a real case
  needs it.

## Open

- The mechanism stays in Rust: the store, generations in `/store/.system`, activation, the init
  that starts systemd, and boot entries. A platform that updates another way, such as A/B
  partitions, needs that mechanism chosen by the platform too. That's its own decision, for when
  it's needed.
- Platform-wide defaults, such as which kernel, if a platform needs them.
