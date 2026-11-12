# 0030: Modules can be turned off, replaced, and come from a source

Status: proposed, 2026-11-12.

## Context

Every configuration gets every built-in module, from the `modules/` next to the distro binary.
A module either runs always or behind its own `enable` option. Nothing lets a configuration say
"not this module" or "this module instead", and the only way to bring other modules is
`--modules DIR`, which replaces the whole set and isn't recorded in the lock.

That rules out building something quite different on the same tool: an appliance without
`accounts`' sudo and PAM, a system with another init or network stack, a downstream distribution
with its own base. The adversarial review found the same from the other side:

- `base-system` always adds bash, openssh, less, getty and sudo, and `accounts`, `polkit`,
  `dbus`, `pam-base` and `networkd` can't be turned off.
- A file a module owns, such as `/etc/sudoers`, can't be taken over by the configuration.
- Some of the base lives in Rust where no module reaches it: `BASE_PACKAGES` (bash, coreutils,
  systemd and the rest), the init and switch scripts, branding in os-release and boot entries.

## Decision

- `[modules]` in the starting file says which modules a configuration uses:

  ```toml
  [modules]
  disable = ["networkd", "accounts"]
  from = [{ source = "acme", path = "modules" }]
  ```

- `disable` leaves a built-in module out. Its options go with it, so setting one is an error
  that says the module is disabled. Whatever wrote to the options it read, such as
  `networking.firewall.allowed_tcp_ports`, is an error too, pointing at both: nothing silently
  drops a value.
- `from` adds the modules in a source's directory. The source is pinned in the lock like any
  other (0009), so the model says exactly which module code ran.
- A module from a source declares options under a name of its own, the source's or its module's,
  and can't declare one a built-in already has. To take a built-in's place it says so:
  `module(name = "accounts", replaces = "accounts", ...)`. It then gets the built-in's options
  and must provide what others write to them. Replacing without saying so is an error, so two
  modules never fight over a name by load order.
- The base that's in Rust moves into modules: `BASE_PACKAGES` becomes what `base-system` adds,
  and os-release, boot entry titles and the ESP directory name come from options with today's
  values as defaults. Rust keeps the mechanism (0026): activation, the init that starts systemd,
  the store.
- What a system can't do without stays a check: a system with no init, no `/bin/sh` or no
  `PATH` fails `check` with what's missing, whichever modules are left.

## Consequences

- A downstream distribution is a source with modules and recipes, and its users' configurations
  say `from = [{ source = "acme", ... }]`. It can pin which distro it was tested on through its
  own lock, which still needs `acme`'s lock to be read (open).
- `--modules DIR` stays for developing distro itself, and the model records that it was used.
- The plan shows modules added, replaced or disabled, like any other change.
- Recipe ids and options keep one namespace each for now. Two sources declaring the same recipe
  id is an error naming both; namespacing recipes by source (`acme#jq`) is separate.

## Open

- Whether a disabled module's packages and files leave on the next switch (yes, as any removal),
  and how the plan says so clearly enough for `accounts`, which takes the machine's users with it.
- How a replacement says it provides what the built-in did for other modules, such as
  `security.pam.services.*`: by declaring the same `writes`, or a list of what it provides.
