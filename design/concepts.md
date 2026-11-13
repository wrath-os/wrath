# Concepts

Every feature should be explainable with the words on this page. A feature that needs a new word
is a reason to stop and talk about it before building it. A word that only applies to one kind
of configuration (only the system, only projects) is the same kind of warning.

Words marked "planned" aren't built yet.

## Configurations

A **configuration** is a directory of TOML files that describes what should be there: packages,
files, services, users and their settings. It is the only place where intent lives. Commands
never keep state outside it; trying something leaves no trace, and keeping it writes TOML.

Configurations come in two sizes, all the same format with the same modules, package names,
lock and commands:

| Size | Where | Describes |
|---|---|---|
| system | `/etc/wrath` | the machine, and the homes of the users on it |
| project | `wrath.toml` in a project | the packages, environment and (planned) services a project needs |

The configuration a command acts on is its **context**: `-C DIR`, or the nearest project, or
the system. Output always says which one it is.

A **role** is a kind of machine: a system configuration with `roles/worker.toml` and
`roles/laptop.toml` instead of `system.toml` describes two. Every machine with a role runs the
same system; commands pick one with `--role`, and `check` and `lock` cover every role.

## Describing what you want

An **option** is one typed setting with a dotted name, such as `services.nginx.main.port`. It
has one value. Two files setting it differently is an error, never a silent override. `wrath why`
says where each value comes from.

A **default** is a value others may change, written `{ default = "Europe/Stockholm" }`. Every
value in a file included with `{ path = "common.toml", as = "defaults" }` is a default too. A set
value beats a default, and when two defaults disagree, the file that includes the other wins.

The **starting file** is the file a configuration starts from, such as `system.toml`, a role's
file, or (planned) a project's `wrath.toml`. It owns the configuration: included files can't
change it, and `add` writes to it.

A **module** declares options and turns their values into packages, files, services and settings.
Modules are written in Starlark and can share code through **libraries**. Users write TOML, and
only write modules to add something new.

A **package** is software to install, named `[source#]name[@version]`: `jq`, `stable#jq`,
`jq@1.7` (planned), or `github:jqlang/jq#jq`. It's written the same way in TOML and on the
command line.

A **build** is a package built differently from how its source builds it, with other values for
its build options or with patches: `[builds.rescue-nano]` with `package = "nano"` and
`options.enableTiny = true`. The options are the ones the recipe declares, each with a type and a
default. The name goes in `packages` like any other. Only that package changes.

A **source** is where packages, or files to include, come from: a name and the URL it follows,
pinned in the lock. `wrath` is the default: its recipes build every package and the base
system. A file from a source is included as `{ source = "acme", path = "rust-service.toml" }`.

A **platform** is the set of modules a system builds on: `wrath`'s built-in ones, or a
source's, listed in its `platform.toml` with the built-in ones it keeps. The starting file
chooses it with `platform = { source = "acme" }` (0030).

A **file** is a file the configuration writes, on the system or in a home. It is text, or settings
written out as JSON, TOML or systemd's format, and a system file can restart what reads it. A file
whose text includes secrets is filled in on the machine, so the secret never reaches the build.

A **secret** is a value encrypted with age next to the configuration, decrypted only on the
machine (for a home of its own, by its user), and referenced as `{ secret = "name" }` or
`${secret.name}`.

A **service** is a program that's kept running, or a task run once. A machine's services are its
operating system's own: on Linux, systemd **units**, from modules or written under `[units]`
with systemd's own sections. A home's or a project's services are written the same way
everywhere, with a command, environment, restart policy, readiness and the services they need
(projects run them with `wrath up`).

A **home** is what a configuration puts in one user's home directory: files, packages, services
and environment variables.

A machine's **state** is what survives a reboot when its root is erased at every boot. With
`state.path` set, `/` is memory, and only the `directories` and `files` listed under `[state]`, or
under `[state]` in a user's file, are kept, under that path.

A **machine value** is something that differs between machines running the same configuration:
a **fact** the machine detects, such as `${facts.cpu.count}`, or a value it's given, such as
`${machine.name}`. Like secrets, machine values are filled in on the machine when it switches,
so they only go in options that are filled in there too, and the build is the same everywhere.
Values other than the name are declared in `[machine.values]`, as `zone = { type = "string" }`.

## Making it so

The **lock** pins every source, wrath's own among them, so the same configuration always
means the same software. It lives next to the configuration as `wrath.lock`. It also keeps
each user's uid and each group's gid, so they stay put as others come and go.

The **model** is the configuration fully resolved: every option's value and everything modules
produced, with nothing left to compute. Its hash identifies a system.

A **plan** is the difference between two models, usually the running system and the
configuration. Each change has a **tier**, saying when it takes full effect:

| Tier | Takes effect |
|---|---|
| live | right away, by restarting what changed |
| next login | when users log in again |
| soft reboot | after `systemctl soft-reboot`, when the base system changed |
| reboot | after a reboot, for the kernel and bootloader |

A **generation** is a built configuration that can be switched to. `apply` makes a new one,
`rollback` goes back to an earlier one, and `history` lists them. The generation being switched to
does the switching with its own wrath, so a newer or older wrath on either side doesn't matter.
When it can't read the running generation, it only starts what's new, and the rest takes effect at
the next boot.

The **builder** turns a model into a generation: it builds every package from its recipe, from
source, into a content-addressed store, and assembles the system from them. Nothing above this
line depends on how it works.

## Words we keep out of sight

Action, sandbox, closure, view and store path are how the builder works. They appear only in `--verbose` output and in `why`, never in TOML or in
what commands normally print.

## The verbs

| Verb | Does, for any size of configuration |
|---|---|
| `check` | evaluates it and reports errors |
| `plan` | shows what would change |
| `apply` | makes it so, after showing the plan |
| `status` | says what's running and whether it matches |
| `history`, `rollback` | lists generations, goes back to one |
| `lock`, `update` | pins sources, moves them forward |
| `run`, `shell` | uses packages without changing anything |
| `add`, `remove` | edits the configuration's TOML |
| `why` | explains where a value comes from |

A verb that only makes sense for one size, or that does something subtly different per size,
needs a written reason here.
