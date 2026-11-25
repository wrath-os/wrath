# Vision

Where wrath is going, and what the core has to allow so that the rest can be built on it. The
decisions say what's settled; `ideas.md` holds small ideas for later; this file holds the large
ones and how they fit together. Nothing here is a commitment to build in any order except the
last section.

## What wrath is

A toolkit for building operating systems that are declared, locked, built from source and
traceable to their configuration. The aim is that you can build many kinds of system with it: a
finished desktop, an immutable appliance, a fleet of identical servers, or a downstream
distribution with its own base, all on one core, one model and one set of verbs.

The core is small and stays in Rust: the store, the builder and sandbox, keys and the cache,
evaluation and the lock, generations, activation and boot entries. Everything a person might
want different lives above it, in Starlark modules and recipes that a platform (0030) chooses.
Grapes is the reference set built on the core, and its platform is one platform among possible
others.

The five reasons in 0012 are the bar: a familiar language, speed, introspection, a toolkit for
custom systems, and a deterministic, auditable OS.

## Layers

```text
products      a finished desktop | an appliance | a server role | a downstream distribution
                         |
platforms     which modules a system builds on, and how it updates (0030)
                         |
sets          recipes, styles, modules: grapes, or someone else's
                         |
core          store, builder, cache, model, lock, generations, activation, boot
```

A product is a configuration plus a platform, published as a source. Building a new kind of
system should never mean forking the core.

## Staying small

One person has to be able to hold the core in their head and still change it in years. It was
about 44,000 lines of Rust, tests aside, in November 2026; it should stay under 50,000 while it
grows into everything above, which means features mostly land elsewhere or replace code.

- Above the core first. A service, a program or a platform is a module or a recipe. The core
  grows only for what every configuration needs, and the agent and coordinator are packages.
- What the machine already has, it uses: systemd's timers, boot counting, credentials,
  cryptenroll, repart and networkd. wrath writes their files and leaves the running to them.
- A change that adds more than a few hundred lines of core says why it can't be a module and
  what it lets go of. Code no command needs any more, such as what an experiment left behind,
  is deleted rather than kept working.
- Commands are few and each earns its place: a new one is weighed against folding it into an
  existing verb or a flag, and developer tools that aren't for users stay under `dev` or become
  scripts.
- Tests check what a user sees (module renders, golden errors, VM scenarios) more than how the
  code gets there, so code can shrink without rewriting tests.

## Products the core has to allow

### A finished desktop

Taste and a finished first hour matter as much as technology: a keyboard-first tiling desktop
that works on day one, one launcher for apps and system actions, themes done for you,
approachable docs, a bare mode, and a full reset. wrath adds what a description brings: updates
that roll back, settings you can diff, and every piece reviewed in a plan.

On wrath that's a product: one line (`[desktop] session = "niri"` or a named desktop) brings a
curated whole, and every piece of it can be overridden in TOML, diffed in a plan and rolled
back. Bare mode is removing lines. A full reset is going back to a generation, or the root erased
at every boot. What it needs from the core: fast files-only switches (`ideas.md`), modules that
say how a program picks up a change, homes of their own for per-user settings, and an install
flow a person can follow.

### Appliances

One signed immutable image per role, managed through an API with no shell, A/B updates that roll
back on a failed boot, and a machine config applied with modes (live, next boot, try). One
declared state, a content hash naming a system definition, and images as an output. Extensions
rebuild with the system, and a supported debug container, chosen by the platform, is how a
machine with no shell gets looked into.

What it needs from the core:

- A platform chooses how the system updates: generations in place, as today, or A/B partitions
  with a sealed image (erofs or composefs, `store.md`). 0030 leaves this open.
- Images as a build output: a disk image, an installer image, a UKI, named by the hash of the
  configuration and lock that made them, and buildable on any machine with the same answer.
- A remote API for the same calls the CLI makes (`cli.md`), so a machine can be managed with no
  shell, and the plan, try and rollback work the same over it.
- Machine policy (`ideas.md`): checks installed with the machine's key that every configuration
  it's given must pass.
- A base with nothing a role doesn't need: no sudo, PAM or getty when the platform leaves them
  out (0030).

### Servers as cattle

Identical machines per role, updated by rolling a new image or generation out, never edited in
place (0012). What it needs: the cache, so no server builds anything; one configuration for many
machines with per-machine values; staged rollouts that stop when a canary fails its checks; and
a way to see which machines run which generation.

### A downstream distribution

Someone else's base, recipes and modules, using wrath's core and keeping its guarantees: a source
with a platform and recipes, pinned in their users' locks (0030). What it needs: recipe ids
namespaced by source, and their own cache and keys, trusted per cache.

## Fleets

How servers as cattle, appliances and Kubernetes nodes get managed together. It draws on
experience running Kubernetes clusters on rented servers with a fleet manager.

### Desired state stays in git

The configuration repository says what every machine should run: roles, machines, the lock and
secrets encrypted to recipients. A fleet service holds only what git can't: what machines run,
their facts, leases and how far a rollout got. Onboarding a machine is a commit,
`machines/<id>.toml` with the values its facts gave, by CI or as a pull request. Addresses
allocated to machines are pinned in the lock, as uids are (0029). Every change to the fleet is
reviewed and rolled back like a change to one machine, and losing the service loses history,
nothing else. A machine that follows a repository on its own (0040) keeps working without it.

### Pieces

- **The agent**, a service on each machine. It dials out, so it works behind NAT with no ports
  open. It reports the running generation, its commit, health and facts, and takes the calls the
  CLI makes: plan, apply, try, `--boot`, status, logs, reboot, reset. `--host` goes through it.
- **The coordinator**, a small service with SQLite: machines and their keys, channels mapping to
  commits, rollouts in waves with canaries that halt on failure, reboot slots, provider jobs.
- **Providers** for Hetzner Robot, Hetzner Cloud, PXE and qemu, each a separate process holding
  its own credentials. Facts have a common shape plus what each provider adds.
- **Discovery**: a live image boots and its agent reports facts; match rules in the repository
  pick a role (`role = "storage"` when there are two NVMe disks); the machine's file is committed
  and `wrath install` runs from the live image.

### starmole as the transport

starmole connects machines by public key over QUIC (iroh): through a relay first, directly once
hole punching works, with relays and address lookup that can be self-hosted. It's a Rust
library, and a program can be a node serving its own protocol, so the agent and the coordinator
are both nodes speaking `wrath/agent/1`, with certificates, renewal and membership from
starmole.

- A starmole invite is the join token: the discovery image or first-boot source carries one,
  and `revoke` cuts a machine off everywhere within seconds.
- Access rules by tag: only the coordinator's tag may use the agent, admins may open a shell or
  run commands, with starmole's log as the audit trail. An issuer certificate bounds which tags
  the coordinator hands out, so it gives a new machine its role's tag and no more.
- starmole keys are Ed25519 like the SSH host key secrets are encrypted to; whether one key can
  serve both is to find out.
- The agent and starmole are packages built from grapes, not part of the core.

### What a coordinator can't do

It says "go to commit X", nothing more. The machine checks that X is on the repository it
follows and fetches the build from caches it trusts, with its own keys. A compromised
coordinator or mesh can move machines between reviewed, signed states and nothing else.

Nothing at boot waits on the mesh or the coordinator: unlocking disks, booting and running the
system stay local. A network service that a node needs before it can mount its state turns that
service's outage into the fleet's.

### Rollouts follow the plan's tiers

Changes that take effect live or on a restart roll as fast as health checks pass; only reboot
changes take a slot. Roles declare health checks that gate `boot-complete.target`, so a node
that boots but is broken rolls itself back with boot counting. A plan's destructive changes,
such as to disks, wait for approval.

Modules declare guards around disruptive changes, and the core knows nothing of what they
guard: before a reboot, drain the node; before going on, wait for etcd to have quorum. A Ceph or
Postgres role brings guards of its own the same way.

Drift is a generation's hash against the one wanted, and `verify` for the rest.

### Kubernetes nodes

A product on top: recipes for containerd, runc or crun, CNI plugins, etcd and Kubernetes from
source, a kernel with what Cilium needs, drbd and zfs as ordinary packages, and a `kube`
platform with no shell, PAM, getty or sudo (0030). The cluster's PKI is role secrets. Operations
such as bootstrapping etcd, taking a snapshot or fetching a kubeconfig are module commands
(0043), run on a node through the agent: `wrath --host cp1 kube bootstrap`. It
changes nodes without rebooting when the plan allows, takes kernel modules on its own schedule,
and boots only what its own keys signed.

### Order

1. On the machine: `plan --json` and `status --json`, health checks gating boot-complete, a
   first-boot config source, module commands (0043).
2. The agent over starmole, with the machine's identity; `--host` through it.
3. The coordinator: channels, waves, slots, guards.
4. Provisioning: the discovery image, the Hetzner Robot provider, match rules making commits.
5. Kubernetes nodes.

Open: a new machine's key has to join its secrets' recipients, which means re-encrypting with a
key that can decrypt, so either CI holds a role or admin key or a person approves and
re-encrypts.

## Larger directions

These come from looking at what a new operating system in 2026 would have to offer beyond
another set of defaults. They are why the core is shaped the way it is, even where nothing is
built yet.

- **Agents that change systems safely.** An agent proposes a configuration change; the person
  sees the plan, its tiers and a diff, and applies or rejects it; anything applied can be rolled
  back. wrath already has the parts: plan before apply, `try`, generations and the lock as an
  audit trail. The update loop for recipes (an agent bumps a recipe until its tests pass, a human
  reviews) is the same idea applied to the set. Later: agents as principals on a machine, with
  their own identity, scoped grants and an action log, built from namespaces, cgroups and the
  model's knowledge of what each change touches. The machine stays fully usable with no AI
  service at all.
- **A verifiably secure computer.** Reproducible state, atomic updates, signed images, Secure
  Boot with keys made per machine, TPM unlock tied to what booted, and recovery that happens on
  its own (boot counting). The machine can say what it runs and prove where it came from.
- **A sovereign computer.** The person owns the configuration, the keys, the caches and the
  sources. Nothing depends on one company's service: a cache is a directory or a bucket, sources
  are pinned by hash, and every build can be redone at home.
- **A twenty-year computer.** A configuration from two years ago still builds the system it
  described (0012). Old versions stay buildable, old hardware stays supported as long as someone
  writes the recipes, and everything works offline once fetched.
- **A personal fabric.** One configuration repository describing the laptop, the desktop, the
  home server and the edge boxes, sharing one cache, with the same verbs for each.

## Where wrath stands against the hard questions

- **Is it architecturally different?** Yes: its own store, builder, keys, traces and model.
- **Who is it for?** Developers who want a declared, locked system they can roll back, and people
  building systems of their own. The first is a demanding audience; the second is where the
  toolkit wins.
- **How do people move to it?** The weakest point today. It needs desktop apps from Flatpak while
  recipes catch up, homes and project shells that run on other distributions, and an install that
  takes an afternoon.
- **Does it compound?** The cache, recipes kept current by agents, and products others build on
  the core all get more valuable as they grow.

## Order

The core first, because every product above depends on it and a weak core makes them hard or
impossible: installing from a configuration repository, upgrading from a URL, the cache, a
platform choosing its update mechanism, images as outputs, and the remote API. Then one product
done well, the desktop on the Framework, before the others.
