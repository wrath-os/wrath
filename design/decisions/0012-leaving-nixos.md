# 0012: Leaving NixOS, keeping nixpkgs

Status: accepted, 2026-11-08.

## Context

0001 put distro on NixOS so it could be used early, and planned to replace NixOS one layer at a
time. Today most of a machine still comes from NixOS modules:

- The shim maps the model to about 25 NixOS options: users, hostname, networking, the firewall,
  kernel parameters, the boot loader, LUKS devices, file systems, swap, initrd SSH and services.
- Some areas are turned on whole: audio, the login manager and the compositor. Each brings PAM,
  polkit, dbus policy and units that distro never sees.
- Everything around that comes from NixOS too: `nixosSystem` evaluation, activation,
  `switch-to-configuration`, the initrd, boot entries, the PAM, dbus, getty and nscd baseline,
  and `nixos-install`.

What distro wants from this layer, and already has for what it renders itself:

- Evaluation that takes milliseconds.
- Saying ahead of time what a switch restarts.
- Values that merge in a way anyone can follow.
- Errors that point at the user's configuration.

What distro is measured on:

1. A familiar, simpler language: TOML for configuration, Starlark for modules.
2. Speed: evaluating a configuration and updating a system.
3. Introspection: seeing what an update changes, in configuration and packages, before applying it.
4. A toolkit for custom systems: immutable, possibly with A/B boot, such as appliances and servers
   managed through an API with no shell. These aren't built yet, but the design has to allow them.
5. A deterministic and auditable OS, where everything on a machine traces back to its
   configuration and lock.

There are two targets:

- Desktops and laptops, changed in place and often.
- Servers run like cattle: one image per role, identical machines, and
  updates rolled out instead of edited in place.

Both have to stay declarative and traceable. Every package and every file on a machine comes from
the configuration and its lock. Checking out a commit of a configuration repository from two
years ago has to build the systems it described then, unchanged.

## Decision

### What stays and what goes

nixpkgs stays as the package source, and Nix stays behind the backend as the store and the
builder. Replacing the store is a separate decision.

These go:

- the NixOS module system and `nixosSystem`;
- activation and `switch-to-configuration`;
- the NixOS initrd modules;
- `nixos-install`;
- the `nixos` escape hatch module.

### A commit reproduces its systems

- The lock pins every input:
  - the sources it already pins;
  - distro itself, meaning its evaluator, its modules and its factory. A configuration locked with
    one distro is built by that distro, whichever distro is installed. A distro that isn't the
    locked one runs the locked one. `distro update` moves the pin like any other, and the plan says
    so.
- What a machine builds depends only on two things:
  - the configuration and its lock;
  - for a machine with a role, its entry in `machines.toml`.

  What install records on the machine and the facts read from its hardware only check. They
  never change what is built.
- The same lock gives the same system, down to the store path. A scenario builds a configuration,
  then builds it again from a fresh checkout and compares.
- Old sources have to stay fetchable. A pinned revision that its host drops can't be rebuilt from
  source, though a cache that kept the build still has it. Archiving a configuration's sources is
  for later. It is the one gap left.

### The system is assembled from the model

The backend builds a system straight from the model, as a tree:

- the packages;
- `/etc`;
- units, from modules and from the packages that ship them;
- users and groups;
- the kernel, its modules and firmware;
- the initrd;
- the boot entry;
- distro's activation program;
- `model.json`.

No module system sits in between. The lock already records the build of each package, so when a
cache has them, building a system doesn't evaluate nixpkgs at all.

### The base system is distro modules

These are ordinary distro modules that render units and files, and the plan shows what each
renders:

- PAM, which exists already;
- dbus, polkit and logind;
- udev and hwdb;
- name services, setuid wrappers and journald;
- time and locale;
- fonts, graphics drivers and audio;
- login managers.

nixpkgs packages expect a few paths at run time: `/run/current-system/sw`, `/run/wrappers` and
`/run/opengl-driver`. The backend provides these. They're conventions of the package source and
don't show in TOML.

### Activation is distro's, from the model

distro's activator switches from one system to the next by comparing the two models:

- `/etc` files;
- users and groups;
- tmpfiles;
- which units start, stop, restart or reload;
- whether the new kernel or initrd needs a reboot.

`plan` uses the same comparison, so it shows exactly what `apply` will do, unit by unit, with each
change's tier.

`/etc` holds only what the configuration puts there and what programs keep at run time, so a
machine can later have it read-only.

### Booting

- systemd-boot has one entry per generation, with its kernel and initrd copied to the ESP and
  its command line in the entry. The activator writes them, for every generation, on each switch.
- Secrets the initrd needs from the disk, such as its SSH host key, go in a small second initrd
  made on the machine and listed in the entry. They never enter the store, so the main initrd
  stays reproducible and shareable.
- Unified kernel images come later, for Secure Boot signing and TPM policies tied to the image.
  A unified image holds the initrd and the command line, which names the system, so it has to be
  made after the system, and the initrd's secrets need another way in, such as a credential
  encrypted to the TPM.
- The initrd is systemd, built from the model's initrd part the same way as the system: units,
  files and packages. Unlocking and mounting come from the disks module's output, as they do now.

### Installing

`distro install` lays out disks as it does now, then:

1. copies the system into the target's store;
2. makes it the first generation;
3. installs systemd-boot and writes the entry.

### Two kinds of machine, one system

A desktop switches in place. A server role is built once into an image, made with systemd-repart
like install's layout: the ESP, the system and state. Every machine of the role boots the same
image and gets its own values from `machines.toml`. Updates are built systems the machines fetch
and boot into, with the previous one kept for rollback. Nothing is evaluated on the machine. Its
own decision will cover the details. What it needs from everything above:

- builds never read the machine;
- `/etc` is generated;
- state lives under `/var` and `/home`;
- a built system can be copied to a machine as it is.

### Moving over

A second lowering, the native one, sits next to the NixOS one and builds from the same model. It
is chosen with a flag while it grows, and scenarios run on both. When it passes every scenario it
becomes the only one, and the NixOS lowering and the `nixos` module are deleted.

The NixOS lowering gets no new work. The base system is written once, as distro modules marked as
part of the base: the native lowering builds them, and the NixOS lowering leaves them out and
keeps NixOS's own. Scenarios move to the native lowering as it covers what they need.

The steps:

1. The lock pins distro, and a distro that isn't the locked one runs it.
2. The native lowering boots a small system in the VM: the kernel, a systemd initrd, `/etc`,
   units, users, a console login, networking, SSH and the guest agent.
3. Its activator switches between systems, and `plan` shows the same changes. Generations,
   rollback, try and keep work on it.
4. Base modules: dbus, polkit, logind, udev, name services, wrappers, journald, time and locale.
5. Desktop modules: audio, the login manager, the compositor, fonts and graphics. m1 to m3, homes
   and the desktop fixtures pass on it.
6. Disks and boot: boot entries, LUKS and RAID in its initrd, the TPM, initrd SSH, and install
   without `nixos-install`. The install scenarios pass on it.
7. The native lowering becomes the only one. Done: the NixOS lowering and the `nixos` module are
   deleted, and VM scenarios that build a configuration boot it from the host's store.
8. Server images and fetched updates, after their own decision.

## Rejected

- Keeping NixOS modules and generating them faster. Evaluation would get faster, but activation
  and errors would stay outside what distro's model can explain.
- dracut or mkosi for the initrd. dracut is shell scripts with their own conventions, and mkosi
  builds from another distribution's packages. A systemd initrd described by the model is smaller
  and fits the rest.
- An API daemon on servers in place of SSH. It may come later, with the server decision.
- Replacing the Nix store now. The store works; what distro needs to own is the layer above it.

## Consequences

- distro owns the base system's details: PAM stacks, dbus policy, polkit rules and units. It
  carries more code, and mistakes there break logins. Each step is checked by the same VM
  scenarios before it replaces anything.
- Machines that build on an existing NixOS configuration through the `nixos` module have to move
  their remaining Nix into TOML and modules before step 7.
- Running the locked distro means a checkout of any age keeps building, as long as a cache or its
  sources still have the pieces.
- `plan` becomes the full account of a switch.
