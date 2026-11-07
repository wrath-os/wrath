# 0010: Projects

Status: accepted, 2026-11-07.

## Context

A project, such as a repository, needs tools and environment variables to work on it: a Rust
toolchain, `sqlx-cli`, `RUST_LOG`, and later a database to run against. devenv and Nix shells
cover this today, with a second language and a second set of commands. `concepts.md` lists the
project as the third size of configuration, with the same format, modules, lock and commands.

The layering experiment wrote one, `myapp`, including a company template as defaults and with a
personal file that changes the port without committing it. That worked with 0007 as it is.

## Decision

### Files

```text
myapp/
  distro.toml          committed; the project
  distro.lock          committed; pins what distro.toml uses
  distro.local.toml    optional, gitignored; mine
  distro.local.lock    optional, gitignored; what only distro.local.toml adds
```

A command run inside a project finds it by walking up to the nearest `distro.toml`. That's the
second place a command looks, after `-C` (see `cli.md`).

The starting file is `distro.local.toml` when it exists, otherwise `distro.toml`. A local file
normally takes the whole project as defaults, so its owner can change anything:

```toml
# distro.local.toml
include = [{ path = "distro.toml", as = "defaults" }]
packages = ["bacon"]

[services.postgres]
port = 5433
```

### What a project sets

`packages`, `environment`, `sources` and `include`. Services join when 0005 is decided.

`environment` is what homes call `session_variables` today. Homes rename it to `environment`, so
the same thing has the same name everywhere.

### Locks

`distro.lock` is written from `distro.toml` alone, so it's what CI and everyone else gets.
Packages that only `distro.local.toml` adds are locked in `distro.local.lock`, using the source
pins in `distro.lock`. A local file can't change what a source is pinned to.

### Commands

`shell`, `run`, `add`, `remove`, `check`, `lock`, `update`, `why` and `options` work in a
project. `add` and `remove` write to the starting file, as everywhere (0007).

A project has no generations, so `plan`, `apply`, `try`, `rollback` and `history` don't apply.
`distro shell` builds the environment when the model changed since last time, and keeps a GC root
under `.distro/` so `clean` leaves it alone.

### A project inside a home

Inside `distro shell`, the project's packages come first on `PATH` and its variables replace the
home's. That's the one place two disagreeing values aren't an error, so the shell says what it
shadows when it starts:

```console
$ distro shell
project myapp: distro.local.toml over distro.toml
  shadows jq 1.8 with jq 1.7, and EDITOR
a shell with rust, sqlx-cli, jq, bacon; exit to leave
```

### Later

- Services, with 0005.
- Entering the environment on `cd`, as direnv does.

## Rejected

- One lock for both files. Either CI would see personal packages, or it would have to ignore
  parts of the lock.
- Generations for projects. Nothing is switched: a shell is entered and left.
- A separate format or command set for projects, as devenv has.

## Consequences

- Homes rename `session_variables` to `environment`.
- The project's schema is a small root of its own: modules say which roots they apply to.
