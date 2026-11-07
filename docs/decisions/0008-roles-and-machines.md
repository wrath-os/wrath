# 0008: Roles and machines

Status: accepted, 2026-11-07.

## Context

People with more than one machine keep them in one repo, so shared settings are written once.
NixOS needs an output per machine (`nixosConfigurations.<host>`), even when machines differ only
in their hostname. With tens or hundreds of machines that are interchangeable, a file per machine
breaks down, and every machine gets its own build even when the systems are the same.

A configuration today describes one machine, starting from `system.toml`, and facts such as the
CPU count are read when it's evaluated. 0007 says a configuration starts from one file; this
decides which file that is when there are several machines, and what's allowed to differ between
machines that share it.

## Decision

What a machine is and which machine it is are kept apart.

### Roles

A **role** is a kind of machine, described by a starting file in `roles/`. A machine that's one of
a kind, such as a laptop, is a role with one machine. `system.toml` is a configuration with a
single unnamed role, so one machine needs nothing new.

```text
world/
  roles/neptune.toml      include = ["common.toml", "profiles/desktop.toml"]
  roles/worker.toml       include = ["common.toml", "profiles/server.toml"]
  roles/gpu-worker.toml   include = ["roles/worker.toml", "profiles/nvidia.toml"]
  common.toml
  profiles/
  users/                  every role
  services/               every role
  machines.toml           optional
  distro.lock             every role
```

- A configuration has `system.toml` or `roles/`, not both.
- A role is built and locked once. Every machine with that role runs the same system.
- `users/` and `services/` apply to every role, as they do today.
- `add` and `remove` write to the role file, so a change reaches every machine with that role.
- One `distro.lock` at the root pins sources for every role; `lock` and `update` evaluate each.
- Roles don't choose profiles or packages by hardware. Hardware classes that are known in advance
  become roles of their own, made from profiles.

### Machine values

A **machine value** is something that differs between machines with the same role: its name,
an address, a zone. Machine values come from the machine, never from a role:

- `${machine.name}` and `${machine.<key>}` are values given to the machine, at install or in
  `machines.toml`.
- `${facts.<path>}` are values detected on the machine, such as `facts.cpu.count`.

Like secrets, machine values are filled in on the machine when it switches, so the build is the
same for every machine with a role. They're allowed in the options secrets are allowed in, and an
option that changes the build refuses them. Facts work this way for every configuration, one
machine or many.

`system.hostname` defaults to `${machine.name}`. A machine's name is the one given at install or in
`machines.toml`. A machine that wasn't given one keeps the hostname the network gives it. Its
current hostname can't stand in for a name: at boot, before anything sets it, there is none.

A role declares the machine values it needs, so a missing one is an error at `check` or
`install` rather than on the machine:

```toml
[machine.values]
zone = { type = "string" }
```

### Which role a machine runs

In order: `--role NAME`; the role recorded on the machine at install; the machine's entry in
`machines.toml`. `distro install --role worker --set zone=eu-north-1b` records the role and
values in `/var/lib/distro/machine.toml`. A configuration with a single role uses it without
asking, so moving from `system.toml` to `roles/` starts as a rename.

That file is state outside the configuration, an exception to intent living only in TOML. It
holds the machine's identity, which the machine owns, and nothing about what should run on it.

`machines.toml` names machines in the repo when that's wanted, one line each:

```toml
neptune = { role = "neptune" }
w-gpu-03 = { role = "gpu-worker", zone = "eu-north-1b" }
```

Machines that aren't listed get their role and values at install, which is the usual case for
machines that are interchangeable.

A machine is found in `machines.toml` by its recorded name, or else its hostname. The role it was
installed as and its entry must agree. Its entry's values are written to
`/var/lib/distro/machine.toml` when it switches, so the repo decides them. `check` makes sure each
machine has a role and exactly the values its role declares. A configuration in `system.toml` has
no `machines.toml`: it has one machine, and what differs goes in the configuration itself.

### Requirements

A role can state what a machine needs to run it. Each machine checks this before switching, at
`install`, `apply` and `try`:

```toml
[machine.requires]
gpu = "nvidia"
memory_gb = { at_least = 64 }
```

```console
$ distro install --role gpu-worker
error: this machine doesn't fit the role gpu-worker
  gpu: the role requires nvidia, this machine has amd (roles/gpu-worker.toml:5)
```

Requirements don't change the build. They're the same kind of check as machine policy (in the
ideas list), which belongs to the machine instead of the role, and the two should share their
checks (`is`, `at_least`, `within`) when policy is built.

### Other machines, for now

`check`, `build`, `lock`, `why` and `options` work with any `--role`. `plan`, `apply`, `try` and
`status` compare against the running system, so they work on this machine only, until distro can
reach other machines.

## Rejected

- A file per machine, as NixOS does. It doesn't scale to interchangeable machines, and gives each
  machine its own build.
- A directory per machine (`hosts/neptune/system.toml`). Shared users and services would need an
  include in every machine.
- A lock per role or machine. Machines would drift apart.
- Choosing profiles by hardware ("this profile when there's an NVIDIA GPU"). The build would
  depend on the machine, so machines with the same role could run different systems.
- Facts read when evaluating for single machines and at switch time for roles. One rule for both.
