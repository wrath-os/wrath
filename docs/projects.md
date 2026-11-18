# Projects

A project is a `wrath.toml` in a repository. It says which packages the project needs, which
environment variables to set and which services to run while you work on it. Same TOML, same
package names, same lock as a machine's configuration, just smaller: everyone who works on the
project gets the same tools at the same versions.

Something like:

```toml
# wrath.toml
packages = ["go", "sqlite", "just"]

[environment]
DATABASE = "app.db"

[services.seed]
command = ["sqlite3", "app.db", ".read seed.sql"]
ready = "exit"

[services.web]
command = ["go", "run", "."]
needs = ["seed"]
ready = "tcp:8080"
```

Any wrath command run from inside the project, or a directory below it, acts on the project
instead of the machine. The first line of output says so:

```console
$ wrath check
project myapp
no errors: 14 options, 3 packages, ...
```

## A shell with the project's tools

```sh
wrath shell
```

That starts your shell with the project's packages first on `PATH` and its environment set, and
tells you what it hides from your own setup (a different `go`, say). `exit` and you're back.
Nothing gets installed. `wrath shell ripgrep` adds a package on top, just for that shell.

`wrath run` works too: `wrath run jq -- --version` runs a package's program, in the project
if that's where you are.

## Services

```sh
wrath up
```

That runs the project's services in dependency order until you stop it with `Ctrl-c`. Above,
`seed` runs to completion first (`ready = "exit"`), and `web` starts once it has. `ready` can
also be `tcp:PORT`, which waits until something listens there, or `started`, the default.

Under the hood `wrath up` uses [process-compose](https://github.com/F1bonacc1/process-compose),
which is a really good tool for exactly this. A service that isn't meant to start on its own
gets `autostart = false`.

## Your own additions

Sometimes you want something in the project that nobody else on it needs, like your favourite
debugger. Put it in `wrath.local.toml` next to `wrath.toml` and keep that file out of git:

```toml
# wrath.local.toml
packages = ["lazygit"]
```

What it adds gets pinned in its own `wrath.local.lock`, so the shared lock stays the same for
everyone.

## Locking

Projects have a `wrath.lock` like machines do. `wrath check` pins anything new, and
`wrath update` moves the pins forward. Commit it, and everyone on the project gets the same
versions of everything.

## What's not there yet

- Building the project itself. Projects describe the tools around your code, and building the
  code is up to your own build tool for now.
- Projects can use a handful of modules (packages, environment, services). Most of what machines
  have, like `files`, isn't there for projects.
