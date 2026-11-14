# 0036: wrath, grapes, and your own repository

Status: proposed, 2026-11-13.

## Context

One repository holds everything: the `wrath` command (evaluator, builder, activation), the
recipes and build styles, the built-in modules, the docs and the tests. The lock pins a source
called `wrath`, which means "the modules and recipes next to this binary". That has two costs:

- Only a checkout can build. The wrath a machine runs has its modules but no recipes, so it can't
  plan or build, and changing a machine needs `apply --host` from somewhere with a checkout.
- The tool and what it builds move together. Updating the packages means updating the tool, and
  a configuration can't pin one without the other.

NixOS splits these: Nix is the tool, nixpkgs holds the packages and the operating system's
modules, and each user's configuration pins a nixpkgs. Flakes made that pin a lock, after years
of channels and `NIX_PATH`. The aim here is the same split, with the lock from the start.

## Decision

### Three parts

- **wrath**, the tool: the evaluator and the Starlark host API, the builder and store, the
  activator, the commands. It ships no modules and no recipes.
- **grapes**, the set: recipes, build styles, modules, the platform (0030) that lists them, and the
  base system's defaults. It's a source like any other (0009, 0028), at `wrath-os/grapes`.
- **Your repository**: a configuration that names grapes as its platform's source, and the lock
  that pins it, as `~/Development/machines` is today.

```toml
platform = { source = "grapes" }

[sources.grapes]
url = "git+https://github.com/wrath-os/grapes"
```

A configuration that names no platform gets grapes, so the common case writes nothing. Without
`[sources.grapes]`, it follows `github:wrath-os/grapes`, and its lock pins that the first time a
command reads it, as it pins any source: the TOML stays empty, and the lock still says exactly
which grapes built the system. A directory given for one command (`--source grapes=DIR`, or
`WRATH_GRAPES` while working on grapes) builds in its place, and says so when the lock pins
another version.

A system carries the grapes it was built from, as a package every system has, linked at
`/run/system/grapes`, and its model records the pin. wrath on the machine uses that copy when the
lock pins the same version, so it evaluates offline; it never uses another version in place of
the pinned one. A pin it can't have is an error saying how to get it, or how to choose the
system's own copy for one command (`--source grapes=/run/system/grapes`). Installing a system
that's built already, as from an image, takes its disks from that system's model, and needs no
grapes.

### Packages and modules are separate choices

grapes offers two things: its packages (recipes and build styles) and its platform (the modules
a system is made of). A configuration picks them apart. One that builds something of its own
names its own platform, from its repository or a directory, and still gets grapes' packages:

```toml
platform = { source = "acme" }       # acme's modules
# packages without a source in their name still come from grapes
```

A platform can take any of grapes' modules by name, as 0030's list of distro's modules does
(`from-grapes`), so one that changes only a few modules lists the rest.

### Where things live

| | wrath | grapes |
|---|---|---|
| what | the `wrath` command: evaluator, host API, builder, store, activator | what systems are made of |
| holds | `crates/`, the tool's docs and design, acceptance, VM scenarios and their harness, bad configurations with their errors | `recipes/`, `styles/`, `modules/`, `platform.toml`, `seed.pin`, `sources.lock`, module tests, the options reference, packaging findings |
| work | the store a checkout builds into (`build/work`), apart from either repository | |

The store and the builder's records are the tool's: they don't live in the set, so the set can be
a read-only copy fetched by its lock.

### The tool fetches the set

wrath fetches grapes at the revision the lock pins, evaluates the configuration with its modules,
and builds with its recipes. So any wrath can build, on a laptop or on the machine itself, and the
machine needs only the tool, the lock and network: no checkout. With a cache (0018), it fetches
what was built already instead of building it.

### The host API has a version

What modules and styles call (`ctx`, `types`, `formats`, `style()`, `option()`) is the contract
between wrath and grapes. grapes says which version of it it needs. A wrath that doesn't have it
says so, naming the version to get, before evaluating anything. A change to the API that breaks
modules is a new version, never a silent one.

### The activator comes from the set

What switches a machine (the first program, switch, homes) runs from the system's own `wrath`
package. grapes has a recipe for wrath, pinned to a revision of the tool, as nixpkgs has one for
Nix. A machine always runs the activator its system was built with, whichever wrath built it.

### Working on grapes and a configuration together

One flag points a source at a directory for one command, without touching the lock:

```sh
wrath apply --source grapes=../grapes
```

It replaces `--modules` and `--root`, which do a narrower version of the same today.

## Order

1. One repository still, with the concept split: no built-in modules, the set reached as a pinned
   source even when it's this repository's own `modules/` and `build/`, the API version, and
   `--source NAME=DIR`. Machines build from the fetched set.
2. The repositories split, once the host API stops changing every week: `wrath-os/wrath` and
   `wrath-os/grapes`. With the concept split already, this is mostly moving files. Module tests
   and VM scenarios move to grapes, against a pinned tool.
3. Configurations applied from their own repositories, locally, or by the machine pulling its own
   (later, its own decision).

## Consequences

- A change that needs both a tool feature and modules using it lands in wrath first, then in grapes
  with the new API version. Today's homes fix, which touched the builder, the activator and the
  modules, would have been two commits in order.
- `wrath update` moves grapes like any source, and a rollback puts the previous grapes back with
  the rest of the generation.
- A downstream distribution (0030) is a platform built on grapes' modules, as before.

## What others got wrong, and what this does instead

- Nix: one giant file of every package that conflicts on every change, and channels that move
  under you. grapes has a file per package (0034) and is reached only through a lock; the host
  API's version turns "this nixpkgs needs a newer Nix" into an error up front.
- Bazel and Buck2: rule sets in repositories of their own with their own releases, so rules and
  what they build drift apart, and two dependency systems (WORKSPACE, then modules). Recipes and
  modules live together in grapes, and there's one kind of dependency: a source pinned in a lock.
  Toolchains are recipes like any other.
- Kubernetes: many kinds of objects and versioned API groups to say one thing. One model, few
  concepts, the same verbs for a system, a home and a project.
- Talos: an immutable core, but only reachable through its own API. Plain files, `why` and
  `plan` before `apply`.
- Container images: layers without provenance, and tags that move. Content addresses, and every
  version in a lock.
- Most package managers: one mutable global state, shaped by the order things were installed.
  Generations, rollback, and nothing installed outside the model.

## Alternatives

- Modules in a repository of their own, apart from the recipes. Rules and what they build would
  drift, as Bazel's do; a module names packages, units and files that change with the recipes.
- Keep one repository and one version. Simplest, but only a checkout builds, and the packages
  can't be pinned apart from the tool.
- Channels, a set named by a moving branch. Nix's history says the lock is worth having first.
- Modules with the tool and recipes in the set. The modules are what a system is, and they change
  with the recipes, not with the evaluator.

## Open

- Where the cache for grapes is served from (wrath.build is the obvious place).
- Whether grapes is versioned by revision alone or also by release branches, as nixpkgs is.
