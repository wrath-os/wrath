# The command line (draft)

A draft of the command set, so new commands fit one scheme. Nothing here is final; "What's
built" near the end says how far along it is.

## Principles

- One tool, one verb per job. The same verb works for the system, a user and a project; which
  one it acts on comes from where you are, and the first line of output says which.
- Plain words: package, source, lock, plan, generation. Backend words like derivation, flake,
  attribute path and closure only show up in `--verbose` output and `why`.
- Anything that changes a machine shows the plan first and says when the change takes effect:
  now, at the next login, after a soft reboot, or after a reboot.
- Trying something never changes anything. Keeping it writes it into your TOML, so nothing a
  command does lives outside the configuration.
- Every command has `--json`, and the CLI is a thin layer over the same calls a GUI or a remote
  API would make.

## Where a command acts

A command acts on one configuration, its context, found in this order:

1. `-C DIR`, a configuration directory, or `-C URL`: `github:owner/repo[/ref]` or an `https://`
   archive, with `//dir` for a configuration in a subdirectory, such as
   `github:johnae/machines//framework`. A configuration from a URL is read as it is, at its newest
   commit, and its lock is never written (0040).
2. The nearest directory upwards with a `wrath.toml`, a project.
3. The system's configuration in `/etc/wrath`.

```console
$ wrath plan
system /etc/wrath (neptune)
packages
  ~ ripgrep (tier: live)
      14.1.0 -> 15.2.0
1 change, tier: live
```

Another machine is managed from where you have a checkout: `plan --host NAME`, `apply --host NAME`
and `try --host NAME` build here, copy what the machine lacks over SSH, and switch it with the new
system's own wrath. `NAME` is what `ssh NAME` reaches, so `~/.ssh/config` applies, and
`WRATH_SSH` replaces the `ssh` command, such as `WRATH_SSH="ssh -p 2222"`. The login needs
root, or sudo, which asks for its password once.

```console
$ wrath apply --host root@framework
```

## Naming a package

Everywhere a package is named, on the command line and in TOML, it's written the same way:

```text
[source#]name[@version]
```

| You write | You get |
|---|---|
| `jq` | jq from the recipes of the grapes your lock pins |
| `jq@1.7` | jq 1.7 from the newest version of grapes that had it (planned) |
| `acme#jq` | jq from the recipes in the source you named `acme` (planned) |

### Which jq is `jq`?

The one your configuration would give you. Recipes live in grapes, a source of its own, so the
version of grapes your lock pins decides every package's version and source. `wrath run jq`
builds that recipe, or finds it in the store already, and nothing else changes. It never quietly
means "whatever is newest today".

Every command that picks a package says where it came from, on stderr:

```console
$ wrath run age -- --version
age from grapes 1d06008, 2026-10-30
v1.2.1
```

The first line names the version of grapes the package's recipe comes from, or says it's from a
checkout of grapes.

### Versions (planned)

`jq@1.7` means the recipe from the newest version of grapes that had jq 1.7, built from that
version. The store is content-addressed, so whatever comes out the same as what you have is
shared, and an older version costs only what differs.

### Trying, then keeping

```console
$ wrath run jq -- --version      # runs it, changes nothing
$ wrath shell jq ripgrep         # a shell with both, changes nothing
$ wrath add jq                   # writes it into your TOML
```

`add` writes what you typed, `packages = ["jq"]`.

## Commands

The machine:

| Command | Does |
|---|---|
| `wrath status` | What's running, whether it matches the configuration, and anything waiting for a login or reboot |
| `wrath plan` | What would change |
| `wrath apply` | Shows the plan, asks, switches, and says when the change takes full effect. `-y` skips asking. Also takes a system built before: `wrath apply PATH` |
| `wrath try` | Like `apply`, but rolls back after five minutes unless you run `wrath keep` |
| `wrath history [N [M]]` | The generations, each with what changed in it; with N, what changed in generation N, and with two, between them |
| `wrath rollback [N]` | Back one generation, or to generation N |
| `wrath build [PKG...]` | Builds the configuration's system without switching, or the packages named, and prints where each is. `--role` builds another of its roles, `--image` a disk image, and `-o PATH` links the result there, and `--check` runs packages' smoke commands and their own tests |
| `wrath clean` | Removes old generations and what only they used, after showing how much it frees, what builds made that nothing left uses (`--raw`: all of it), and disk images of systems it removed |

Packages:

| Command | Does |
|---|---|
| `wrath run PKG [-- ARGS]` | Runs a package without installing it |
| `wrath shell [PKG...]` | A shell with those packages; with none in a project, the project's environment |
| `wrath up` | Runs a project's services, in dependency order, until you stop them |
| `wrath add PKG...` | Adds packages to the configuration and locks them; `--home USER` for a user's own |
| `wrath remove PKG...` | Removes them |
| `wrath search TEXT` | Finds packages by name and description (not built yet) |
| `wrath info PKG` | Versions, where each comes from, what's installed and what a change would download (not built yet) |

The lock:

| Command | Does |
|---|---|
| `wrath update [SOURCE...]` | Moves sources to their newest versions; single packages are planned |
| `wrath update --list` | Each source with what it follows, where it's pinned and how old that is |

There's no `lock` verb: `check`, `apply` and `add` pin whatever is new. `--locked`, on any
command, writes no lock: the configuration's or a recipe's, and fails where one would change.

Recipes, styles and modules come from grapes (github.com/wrath-os/grapes), a source the lock
pins on its own as `grapes`. A configuration needn't declare it: the lock pins
`github:wrath-os/grapes` the first time a command reads the configuration, `[sources.grapes]`
points it elsewhere, `wrath update grapes` moves it, and `--source grapes=DIR` (or
`WRATH_GRAPES`) builds from a checkout for one command. Every system carries the grapes it was
built from at `/run/system/grapes`, which commands use offline when the lock pins that same
version, and never in place of another.

The lock pins wrath too, as the source `wrath`: its evaluator and builder. `wrath update` pins
the wrath you're running, and `[sources.wrath]` says where wrath is fetched from
(`github:wrath-os/wrath`). A version of wrath other than the one the lock pins says so.

A newer grapes can rebuild packages the configuration never mentions, such as a kernel with
other options. Each system records which packages it was built from, and `plan` compares that
with today's recipes, so a rebuilt package shows with the tier it takes:

```console
$ wrath plan --host framework
packages
  ~ linux (tier: reboot)
      rebuilt
  + bubblewrap (tier: live)
2 changes, tier: reboot
```

Understanding the configuration:

| Command | Does |
|---|---|
| `wrath check` | Evaluates the configuration and reports errors, without building, and pins anything new in it. With `--locked` it fails instead of pinning, for CI |
| `wrath why THING` | Where an option's value, a file such as `/etc/nginx/main.conf`, or a package comes from |
| `wrath options [PREFIX]` | The options there are, with their types, defaults and docs |

Secrets: `wrath secret edit NAME` and `wrath secret rekey`.

Binary caches (0038): `wrath cache key NAME FILE` makes a signing key and prints its public half;
`wrath cache push DIR --key FILE` pushes everything built here that the cache lacks. Any command
that builds takes `--cache URL` (a directory or http(s)) and `--cache-key KEY`, repeatable, and
fetches a package signed by a trusted key instead of building it. The flags stand in until the
configuration says where caches go.

Setting up a machine: `wrath install -C DIR [--disk NAME=/dev/...] [--role NAME] [--name NAME]`,
from wrath's image or a checkout with its builder, lays out the disks the configuration describes with
systemd-repart, encrypts them with a passphrase it asks for, builds the system onto them and
records the machine's role and name. It refuses disks that have partitions unless given `--wipe`.
The machine's SSH host key and initrd key, kept encrypted in `secrets/machines/NAME/`, are
decrypted into it with `--identity FILE`, which stays where install runs. Without `--disk`, it finds
the disks the configuration describes by kind and size, refusing to guess when more than enough
fit, and records them on the machine.
With `[unlock]`, install enrolls the TPM, and the initrd can be unlocked over SSH.

## Output and the API

- Human output by default. `--json` gives the same thing as data, with a schema per command.
- Builds show a line each as they start. On a terminal that line stays up to date with the
  build's time and latest output, and ends as `built NAME 42s`. `-v` shows everything each build
  prints. Each build's full output is kept in `build/work/logs/f/NAME.log` either way.
- Exit codes: 0 for success, 1 for errors. `plan --exit-code`, returning 2 when there are
  changes, is planned but not built.
- `apply` asks before switching unless `-y`. Without a terminal, it refuses rather than
  guessing.
- `wrath options --schema` prints the options as a JSON Schema, so an editor completes, checks
  and explains the TOML as you write it. `--schema users.john` is the one for files in `users/`,
  and `--project` the one for `wrath.toml`. With taplo, in the configuration's `.taplo.toml`:

  ```toml
  [[rule]]
  include = ["system.toml", "services/*.toml", "roles/*.toml"]
  schema.path = "./wrath.schema.json"

  [[rule]]
  include = ["users/*.toml"]
  schema.path = "./users.schema.json"
  ```

- Each command is a call on a library, and later on a local daemon. The CLI, a GUI installer,
  editor plugins and a Talos-style remote API all use the same calls.

## For contributors

The commands that only make sense in this repository move under `wrath dev`: `dev test`
(running scenarios), `dev acceptance`, `dev bench`. `fill-templates` stays hidden;
activation runs it, and people don't.

`dev docs` writes the reference pages in `docs/reference` from the code, and `--check` fails
when they're out of date. `dev lint` records what each recipe's style makes where its lock is
behind (`--locked` fails instead, for CI), and points out recipe files that aren't where they go
and a target written in recipes where a placeholder says it; with `--redundant [PKG...]` it lists
dependencies another one brings, for information. `dev outdated` compares recipes with upstream releases, `dev repro
[PKG...]` builds packages a second time and says which differ, and `dev seed
check|pack|addresses` works on the seed.

`dev test DIR` checks each configuration in `DIR`. One with an `expected/` directory renders:
each file there must be what the configuration writes at that path, with units under
`etc/systemd/system/` and a user's files under `home/NAME/`. Any other case is a bad
configuration whose errors must match its `expected-error.txt`. `--bless` writes what they
actually give. The modules' own cases are in `modules/tests/`; start one with empty files at the
paths to check, then bless and review them.

## What's built

Built: `-C` and the system default, `status`, `plan`, `apply` (asking first), `try`, `keep`,
`history`, `rollback`, `clean`, `build`, `apply PATH`, `run`, `shell`, `add`, `remove`,
`update`, `check`, `install`, `why` for options, files and packages, `options`,
`secret`, the `dev` commands, `--json` for `plan`, `status`, `history` and `update --list`, and
`--role` for configurations with `roles/`, which defaults to the machine's role from install or
`machines.toml`. `apply` and `try` check `[machine.requires]` first, except for a system built
elsewhere (`apply PATH`), which shows its plan and asks but whose requirements aren't recorded
yet. Projects: found from the current directory, with `shell`, `up`, `run`, `add`, `remove`,
`check`, `update` and `why`, and `wrath.local.lock` for what only `wrath.local.toml` adds.

A home of its own, `home.toml` in `~/.config/wrath`, isn't supported for now: a user's
packages and files go under `[users.NAME]` in the system's configuration.

Not yet: versions (`jq@1.7`), `search`, `info`, `update PKG`, `plan --exit-code`, and `--json`
elsewhere.
`clean` lists the generations it removes rather than estimating the space first, and says what it
freed afterwards.

## Open questions

- How a machine builds what it needs, or fetches it built, without a checkout.
- Whether `update PKG` moving one package without moving grapes is worth what it costs: that
  package then comes from a different version of grapes than the rest.
- How long `try` waits by default, and whether it rolls back when the machine loses the network.
