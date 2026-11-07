# 0007: Composing configurations

Status: accepted, 2026-11-07. Extends 0006, whose "left out on purpose" section this replaces.

## Context

A configuration is often built from parts that different files, or different people, own: a
shared base for several machines, a security baseline, a company's project template, one
developer's local tweaks. Each part needs a way to say which of its values others may change.

Within one configuration today, lists merge as sets or ordered lists, `{ remove = [...] }` takes
items out and errors when they aren't there, two values that disagree are an error naming both
files, and `{ value = ..., force = "reason" }` overrides on purpose. Modules can set other options
at a default level with `ctx.default`, and options have declared defaults.

We surveyed how others compose configuration and wrote three configurations as if the candidates
existed: Kubernetes nodes with a security baseline, one person's laptop and server, and a
project's development environment.

- Overlays where a later layer wins (kustomize, Helm values, Ansible's 22 precedence levels,
  systemd drop-ins) hide changes: when a base changes a value an overlay also sets, nobody
  notices. Kustomize's patches also depend on merge keys the reader can't see, and a patch that
  matches nothing does nothing.
- Numeric priorities (NixOS's `mkDefault`, `mkForce`, `mkOverride 60`) work, but people misread
  them, and nixpkgs keeps adding levels in between.
- CUE's unification is closest to what we have: concrete values can't be overridden, and there's
  one default level. But two defaults that disagree cancel out, so a base, a host and a user can't
  each provide a default.
- Policy engines (OPA, Kyverno) check the rendered result, so their errors land far from the line
  that caused them.

## Decision

Values have five strengths, from weakest to strongest:

| Strength | Written as | Loses to |
|---|---|---|
| declared default | an option's `default` | anything that sets the option |
| module default | `ctx.default(...)` in a module | defaults in configuration files, set values |
| default | `{ default = x }`, or a file included as defaults | set values; the including file's defaults |
| set | `x` | nothing; set values must agree |
| forced | `{ value = x, force = "reason" }` | nothing; forced values must agree |

A whole file is included as defaults with:

```toml
include = [{ path = "common.toml", as = "defaults" }]
```

Rules:

- Set values must agree, as today. Disagreeing is an error naming both lines, with help that
  suggests `{ default = ... }` or `force`.
- A set value beats any default.
- When defaults disagree, the file doing the including wins over the file it includes, however
  deep. Defaults from two files where neither includes the other must agree, or it's an error.
- Set and ordered lists merge everything from module defaults to set values. A forced list
  replaces them, and a declared default only applies when nothing else sets the list. `remove`
  removes items whatever strength added them. `{ default = [...] }` on such a list is an error,
  since it would mean the same as the list itself.
- References (`${services.postgres.port}`) see the final value. They stay references: no
  arithmetic, functions or conditionals in TOML. Logic goes in modules.

The **starting file** owns the configuration: `system.toml`, `hosts/<name>.toml`, a project's
`distro.toml`, or `distro.local.toml` when it exists. Includes only point downwards, so an included
file can't change what includes it. The starting file is where `add` and `remove` write unless
told otherwise, it's named on the first line of output, and the lock sits next to it.

When `remove` takes out something an included file adds, it writes the removal into the starting
file and says so, leaving the shared file alone.

Inside a project's shell, the project's packages and variables shadow the user's home. That's the
one place two disagreeing values aren't an error: the project doesn't change the home, it scopes
over it for the length of the shell.

Examples:

| Starting file | Includes | It may |
|---|---|---|
| `hosts/neptune.toml` | `common.toml` with `timezone = { default = ... }` | set another timezone |
| `nodes/w-gpu-03.toml` | `hardening.toml` as peers | change a security value only with `force` |
| `myapp/distro.toml` | the company template, as defaults | change any template value |
| `myapp/distro.local.toml` | `distro.toml`, as defaults | change any project value; CI never sees it |

## Rejected

- Overlays where a later layer wins. Set values that must agree make a changed base show up as a
  conflict instead of being hidden.
- Numeric priorities. The strengths are fixed, and only one can be written by hand besides set and
  forced.
- Treating an upper owner's set values as policy. In a project, the person lower down owns the
  machine, and their local file has to win.
- Locking values inside the configuration. Whoever writes the configuration can always `force`,
  and set lists take additions from anyone, so locking has to come from outside it. That's
  machine policy, kept in the ideas list for fleets.

## Consequences

- The concepts page gains "default" and "starting file".
- `why` says which default a value replaced and which values came through a reference.
- Repos with several hosts, standalone user configurations, projects and including files from
  sources each need the starting file to be chosen, and get their own short design.
